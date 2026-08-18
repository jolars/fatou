---
paths:
  - "src/lsp.rs"
  - "src/lsp/**/*.rs"
  - "src/text.rs"
  - "src/text/**/*.rs"
  - "tests/lsp.rs"
---

# Language server rules

Scope: `src/lsp/`, `src/text/`, and LSP integration tests.

`src/lsp.rs` is a facade over `src/lsp/*`; **its module doc carries the full
threading rationale — read it before touching the main loop.** CLI entry:
`fatou lsp`.

## Transport

`lsp-server` (rust-analyzer's transport), stdio JSON-RPC, synchronous main loop.
Not tower-lsp: salsa cancellation is a synchronous unwind (`salsa::Cancelled`)
that composes with a sync loop plus thread pools and fights an async `&self`
model. Capabilities are advertised by `server.rs::server_capabilities`,
**negotiated against the client's** — including UTF-8/UTF-16 position encoding.

## Threading

- **The main loop owns no salsa database.** A dedicated **analysis thread**
  (`analysis_thread.rs`) owns the persistent `IncrementalDatabase` and is the
  sole *writer*. This is forced: salsa is strictly single-writer.
- Every analysis splits into a cheap **write-phase** (`&mut db`, on the analysis
  thread: upsert the live buffer) and an expensive **read-phase** (`&db` only)
  dispatched to the **read pool** (`task_pool.rs`, `read_jobs.rs`) holding a
  short-lived db clone under `salsa::Cancelled::catch`. Keep that split: a slow
  read must never block queued work.
- `TaskPool` is purpose-built rather than rayon's global pool, which has no
  priority concept. Background package indexing gets its **own single-thread
  pool** — the one unbounded-duration job must never slot-block a read.
  **Never put unbounded work on the read pool.**
- Requests are **coalesced** (latest version per URI; stale edits dropped) in
  lieu of a debounce. `decide` keeps at most one analysis in flight: a
  strictly-newer edit of the *same* URI cancels the running analysis; a
  *different* pending URI waits its turn and is never cross-canceled. The main
  loop drops publishes for closed or superseded documents.
- A db clone outstanding when the analysis thread writes trips
  `salsa::Cancelled`; that and a cache miss both fall back to a fresh parse.
  **Reads are always correct, only sometimes warm** — never trade that away.
- Per-job `catch_unwind` on the analysis thread (`guard`), the read pool, and
  the main loop's request handlers keeps one malformed request from killing the
  server. Keep new entry points inside a guard.

## Paths and positions

- Convert URIs only through `src/lsp/uri.rs` (`to_path`/`from_path`). Tests and
  snapshots must not assume `/` versus `\`.
- Offsets go through `src/text/line_index.rs`; `didChange` conversion is
  `src/text/edit.rs`. The pure edit machinery lives in `fatou-parser` and is
  **not** mirrored in `src/text` — `crate::parser` is the one path to `Edit`.
- **A live buffer is a `TextBuffer` (`src/text/buffer.rs`), never a bare
  `String`**: it carries the line-start table and patches it per edit. An open
  document is an `Arc<TextBuffer>` shared with the analysis thread and every
  read job, so a handler resolving positions against the live buffer takes
  `&TextBuffer` and calls `text.line_index()`. `LineIndex::new` **rescans the
  whole document** — reserve it for text with no maintained table, which means
  text resolved off the db (`snapshot.file_text*`) and the `compute_*` fallback
  paths, where a full parse dwarfs the scan anyway. Reintroducing a rescan on
  the live-buffer path is a silent regression: it type-checks, because
  `&TextBuffer` derefs to `&str`. `benches/line_index.rs` is what measures it.
- **The text itself is an `Arc<str>`, shared, never copied to hand over.** The
  buffer, salsa's `SourceFile.text`, and the reparse base (`PrevParse.text`)
  hold the same allocation, so the write phase passes `text.text_arc()` (O(1))
  and the base stores a refcount bump. Reintroducing a `.text().to_string()`
  on the dispatch path is the regression to watch for. Where the allocation can
  be shared, the staleness guards (`upsert_file`, `parsed_document`) put
  `Arc::ptr_eq` **in front of** the content compare — never instead of it:
  salsa's setter does no equality check at all, so the *compare* is what keeps
  a no-op write from invalidating every memo, and the `ptr_eq` only makes the
  common case free. `revert_file_to_disk` has no such fast path and wants none:
  its text comes off disk, so the allocations can never be shared. An edit
  rebuilds the string once (`TextBuffer::replace_range`), which is the
  deliberate linear pass a keystroke pays for text and the one place a
  malformed range must still panic rather than splice. `benches/salsa_keystroke.rs`
  measures the whole didChange → upsert → parse path, and is the bench that
  catches a cost added *between* the pieces the other benches time.

## Conventions

- Navigate the tree through the typed AST wrappers, not raw `children()`/
  `kind()` matching. The exceptions are the polymorphic kind-classification
  walkers (`symbols.rs`, `folding.rs`, `semantic_tokens.rs`), where a single
  node dispatches over many kinds and single-kind wrappers would add code.
- `latex_symbols.rs` is **generated** by `scripts/generate-latex-symbols.jl`
  from `REPL.REPLCompletions`; regenerate on a Julia bump, do not hand-edit.
  Both tables are sorted by key and `completion.rs` binary-searches them.
- **An open document is not always Julia.** `Document` carries a
  `DocumentKind`, tagged once at `didOpen`; the environment files
  (`Project.toml`, `Manifest.toml`, and friends) are in the client's document
  selector, so any request can arrive for one. **Reach a buffer through
  `GlobalState::julia_text`, `project_text`, or `manifest_text`, never
  `documents` directly** — those three are the only doors into the read pool.
  A `.toml` is **never** Julia: the two environment flavors get their routes and
  every other one is `DocumentKind::Other`, which answers nothing, so a client
  selector matching one file too many costs an ignored document rather than a
  TOML file in the Julia parser. A
  Julia-backed feature asks `julia_text` and answers `null` otherwise, because a
  hover or a format served off a TOML buffer parses it as Julia; a feature that
  understands TOML asks `project_text` and lands in `project_navigation.rs`. A
  `Manifest.toml` answers **one** request, `documentLink` on its `path` entries,
  which needs neither the library nor the database — everything that does need a
  resolved environment stops at that door, since a manifest's text never reaches
  the database. Their own diagnostics have **two producers**: the buffer
  (text-only checks, edit cadence) and the harvester (a resolved `Environment`,
  resolve cadence). The buffer's set supersedes rather than joins —
  `publish_merged` is the one place that merge lives.
- **A feature the harvest feeds needs a refresh nudge.** The library lands
  seconds after `initialize`, and a client re-requests hints only on an edit or
  a scroll, so a full harvest sends `workspace/inlayHint/refresh` (guarded by
  the client capability, as `workspace/diagnostic/refresh` is). Anything else
  reading the library from an already-open document has the same gap and the
  same fix — bar document links, which the spec gives no refresh request.
- **Formatting answers with line-scoped edits, not a whole-document
  replacement** (`format.rs::line_diff_edits`): the formatted output is
  line-diffed against the buffer so an untouched line keeps the client's cursor,
  folds, and markers. A diff covering more than half the span falls back to the
  single replacement, which is the only case that should produce one. Both the
  full-document and the range path go through it, and whatever comes back must
  reproduce `format` byte for byte — that property is what `format.rs`'s tests
  assert, and `benches/format_edits.rs` is what measures the cost.
- A setting that is a fact about the **machine** belongs in editor settings
  (`config.rs`), not `fatou.toml`; project facts belong in the config file. The
  LSP resolves `fatou.toml` the same way the CLI does so both walks honor the
  same excludes.
- Rename covers symbols **and** files and folders (`rename_files.rs`).

## Testing

`tests/lsp.rs` drives the server over an in-memory connection;
`tests/salsa_incremental.rs` guards that a body edit does not invalidate the
project graph — a regression there shows up here as latency, so keep it green.

**CI tests on Windows.** Unix-style absolute paths (`/work`, `/abs/c.jl`) are
**not absolute on Windows**: `is_absolute()` is false without a drive letter,
and `std::path::absolute` grafts the CWD's drive onto driveless paths. Any test
exercising absolute-path resolution or asserting on `file:` URIs must build
platform-native paths — see the `abs`/`file_uri` helpers in
`src/lsp/document_link.rs`'s tests. Paths that stay relative-joined and are
never asserted on verbatim are fine as-is.
