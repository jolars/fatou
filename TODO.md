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
single-STRING token tier and string-interior edits ride the statement tier;
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

- [ ] Reparse stage 3 (top-level statement tier, the big win): region =
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

- [ ] Reparse stage 4 (precise LSP edits + benches): `reparse_edits`
  chaining per-edit reparse, validated up front against the target text
  (stale slices reject to the `diff_edit` path); return the byte edits
  `apply_content_changes` (`src/text/edit.rs`) already computes and stage
  them through the side-channel from `src/lsp/analysis_thread.rs` (a full-
  replacement change clears pending edits); `parsed_document` tries
  precise edits, then `diff_edit`, then full parse. Criterion bench
  `benches/reparse.rs`: full parse of a ~100 KB corpus vs token-tier
  keystroke vs statement edit vs worst-case fallback.

- [ ] Maybe (deferred): a nested-block tier needs a context-parameterized
  fragment entry point (`public_context`, bracket `end` markers) — a bare
  fragment `parse()` misparses those today; a `StringContent` token-tier
  fast path needs delimiter-derived character guards (triple/raw/
  prefixed). Both are pure optimizations on top of a sound stage 2–4.

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
