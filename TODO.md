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

Token/statement reparse splicing beneath `parsed_document`
(`src/incremental.rs`), à la rust-analyzer `reparsing.rs` and arity's
`src/parser/reparse.rs`. Design notes pinned by investigation (2026-08-04):
strings are multi-token (`StringDelimOpen`/`StringContent`/…), so there is no
single-STRING token tier (stage 2b instead relexes the whole enclosing
literal, which is how string-interior edits reach the token tier);
juxtaposition (`2` + `e10` ⇒ `Float 2e10`) demands a backward join guard
arity never needed; Julia blocks are `keyword…end` with `a[end]` ambiguity
and `parse()` is toplevel-contextual (`public`, bracket `end`), so the
splice unit is the top-level statement, not a nested block; the drive loop
consumes across newlines (trailing-operator continuation) and
`fold_docstrings` is cross-statement, so the statement tier needs boundary
guards in both directions; the four `flag_invalid_*` passes are
subtree-local and emission order is per-statement contiguous, so fragment
`parse()` reproduces them and diagnostics splice as five ordered sequences.
Each stage lands independently with the full suite green.

- [x] Reparse stage 0 (infra, no behavior change): new
  `src/parser/reparse.rs` with `Edit` (byte range + insert text),
  `diff_edit(old, new)` (common prefix/suffix strip, char-boundary
  clamped), and a stub `reparse(prev_text, prev_green, prev_diags, edit,
  new_text) -> Option<Reparsed>` returning `None`; `PrevParse { text,
  green, diagnostics }` side-channel on `IncrementalDatabase` (an
  `Arc<Mutex<HashMap<SourceFile, Arc<PrevParse>>>>` beside `source_map`,
  no-op defaults on the `IncrementalDb` trait so test dbs stay untouched);
  `parsed_document` fetches text first (salsa dependency), tries the
  reparse, falls back to full parse, and stores `PrevParse` last
  (cancellation-safe). Unit tests for `diff_edit` and `apply_edits`
  (UTF-8 clamps, whole-replace, no-op).

- [x] Reparse stage 1 (oracle harness first, TDD): new
  `tests/incremental_reparse.rs` à la arity: tree fingerprint
  (kind@range + token text of every descendant); for each corpus snippet
  × ~200 seeded edits (insert alphabet biased toward hazards: `"`, `"""`,
  `$`, `'`, `#`, `=#`, newline, `;`, `end`, `function`, digits, `e10`,
  `+`, `α`), if `reparse` returns `Some`, its fingerprint and full
  diagnostics vector must equal `parse(new)`'s. Hand-written hazard
  snippets: docstring + function, triple/raw/prefixed strings with
  interpolation, `a[end-1]`, `x'` vs `'x'`, `2x`, `x = 1 +` continuation,
  trailing junk `x y`, toplevel `a; b`, stray closers, unterminated
  literals, `public`/`const` forms. Also a Tenet-4 `debug_assert_eq!`
  (fingerprint + diagnostics vs full parse) inside `reparse` itself.
  Trivially green while the stub returns `None`.

- [x] Reparse stage 2 (token tier): relex-in-isolation splice for `Ident`,
  `Comment`, `BlockComment`, and `Whitespace` leaves (explicitly not
  `StringContent`, newlines, chars, or numbers) via
  `SyntaxToken::replace_with`; a boundary insertion tries both
  `token_at_offset` candidates (left first), so typing at the end of an
  identifier splices. Guards, in order: no newline in deleted or
  inserted text; isolated relex yields exactly one token of the same kind
  spanning all of it; contextual-identifier blocklist (`as`, `abstract`,
  `primitive`, `type`, `typegroup`, `public`, `var`, `in`, `∈`, `isa`,
  `doc`, plus `outer` for future `for outer` support — `where` and
  `mutable` are true keyword kinds, auto-guarded by the same-kind check;
  oracle surfaces omissions); forward join (new text + next source char,
  or a `\n` sentinel at EOF, must not extend the token, e.g. `r` + `"…"`
  or a block comment left unterminated by a nested `#=`); backward join
  (prev leaf + new text must relex to the same two tokens; catches `2` +
  `e10` ⇒ `Float`); no existing diagnostic touching the leaf. Shift
  diagnostics after the leaf by the edit delta. Targeted positive tests
  (assert the tier fired) and negative tests (ident ⇒ keyword, `2x` ⇒
  `2e10`, newline insertion, string-content edit all fall back).

- [x] Reparse stage 3 (top-level statement tier, the big win): region =
  contiguous run of `ROOT` child nodes touching the edit, or the empty
  span at the edit point for trivia-gap insertions (covers typing new
  code on a blank line); fragment reparse reuses public `parse()`
  wholesale (reruns `fold_docstrings` and the flag passes). Guards:
  backward boundary (`parse(prev_sibling + gap + fragment)` must yield a
  first top-level item ending exactly at the old boundary; catches
  trailing-operator continuation and backward docstring folds) and
  forward boundary (`parse(fragment + gap + next_sibling)` likewise;
  catches forward absorption and forward folds). Splice by rebuilding the
  `ROOT` green node's child list; splice diagnostics as five ordered
  sequences (parse-emission order, then the four flag passes): keep
  before-region, drop overlapping, shift after-region by the delta,
  rebase fragment diagnostics by the region start. Targeted tests: edits
  inside a function body, character-by-character typing (fallback while
  the block is open, tier fires once closed), docstring-content edits,
  statement-becomes-docstring (fallback), `const x` flag diagnostics,
  edits at BOF/EOF.

- [x] Reparse stage 4 (precise LSP edits + benches): `reparse_edits`
  chains per-edit reparse, validated up front against the target text
  (stale slices, and chains under two edits, reject to the `diff_edit`
  path); `apply_content_changes` (`src/text/edit.rs`) returns the byte
  edits it already computes, `None` on a full replacement, and they are
  staged through the side-channel from `src/lsp/analysis_thread.rs`.
  `Edit`/`apply_edits`/`diff_edit` moved to `src/text/edit.rs` (leaf
  module) and are re-exported from `parser::reparse`. The side-channel is
  one `Mutex<HashMap<SourceFile, FileReparseState>>` holding base +
  pending chain, so a store advances both atomically: stage appends
  (a coalesced request's edits must survive), `parsed_document` peeks,
  and the store drains the peeked *prefix* unconditionally — draining
  only on success would wedge a file into rejecting forever. The chain is
  bounded where it is staged (16 edits / 64 KiB), since pull-diagnostics
  clients stage per keystroke and demand no parse. `revert_file_to_disk`
  evicts (also fixes the pre-existing unbounded `reparse_cache`).
  `parsed_document` tries precise edits, then `diff_edit`, then full
  parse — the opposite of this item's original wording, because
  `benches/reparse.rs` shows a *failed* wide diff costs 17 ms against a
  6.3 ms full parse (the top-level tier answers a wide span with a
  fragment parse plus both boundary guards), while the chain is 59 us.
  Criterion bench over ~100 KB of JuliaSyntax: full parse 6.3 ms, token
  keystroke 15 us, statement edit 542 us, precise chain 59 us, the same
  change via collapsed `diff_edit` 17 ms, rejected attempt 1.0 ms.

- [x] `didClose` removes the document from `state.documents` but leaves a
  stale entry in the analysis thread's `pending` queue, so a queued
  request can dispatch *after* `revert_file_to_disk` and re-upsert the
  discarded unsaved buffer (`src/lsp/state.rs` `DidCloseTextDocument` →
  `src/lsp/analysis_thread.rs` sync arm). Pre-existing; drop pending
  entries on the close signal. The sync arm is now `on_sync`, which also
  drains `analysis_rx` first — a request sent before the close can still
  be unreceived when the `select!` picks the sync arm — and dispatches
  afterwards, since that drain swallows the `analyze` arm's wake-up for
  the other URIs it picks up.

- [x] `path_for` (`src/lsp/state.rs`) collapses every non-`file:` URI onto
  `untitled.jl`, so two `untitled:` buffers share one `SourceFile` and one
  reparse base. Harmless (the chain validation rejects the interleaving
  and falls back to a full parse) but it thrashes. Now
  `uri::to_path_or_synthetic`: the whole URI, percent-escaped into one
  component under a rooted `fatou-non-file-uri` directory. Also fixes the
  relative `untitled.jl`, which `normalize_path` absolutized into the
  server's working directory, where it could alias a real file (and be
  read from disk by `revert_file_to_disk`). Document links ask
  `uri::is_synthetic` before anchoring a relative `include` to the
  document's directory.

- [x] Reparse stage 2b (`StringContent` token-tier fast path): *not* the
  predicted delimiter-derived character guards. Instead the whole enclosing
  `STRING_LITERAL`/`CMD_LITERAL`/`NONSTANDARD_IDENTIFIER` node is relexed in
  isolation twice — once unedited, to prove isolated lexing is context-faithful
  for that node, and once with the edit applied, which must reproduce the same
  token kinds with the edited content token's end (and every end after it)
  moved by the delta. Taking the delimiters along puts the isolated lexer in
  the right mode for free, so triple/raw/prefixed/command/`var"…"` need no
  hand-written hazard table that could drift from the lexer. That also
  *subsumes* both join probes, which the string path skips: the bytes before
  the node are unchanged (backward), and the proven-identical tail — close
  delimiter and any suffix — restores the mode stack (forward). Requires a
  matching close delimiter after the leaf, since an unterminated literal has no
  such tail. Bails when any diagnostic touches the whole literal, hoisted above
  both relexes because an unterminated literal's node is the rest of the file.
  Sound because nothing outside the lexer reads `StringContent` *text*
  (triple-quoted dedent lives only in the test-only sexpr projector), so an
  unchanged token-kind sequence is an unchanged tree and unchanged diagnostics.
  Unlocks docstring keystrokes at the token tier: 18.2 us against the 548 us
  the statement tier charged, since `fold_docstrings` made the docstring and
  the definition it documents one `ROOT` child (`benches/reparse.rs`,
  `docstring_keystroke`). The newline ban still routes Enter-in-a-docstring to
  the statement tier.

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
