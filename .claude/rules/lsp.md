---
paths:
  - "src/lsp.rs"
  - "src/lsp/**/*.rs"
  - "src/text.rs"
  - "src/text/**/*.rs"
  - "tests/lsp.rs"
---

# Language server rules

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
  pool** — the one unbounded-duration job must never slot-block a read
  (`TODO.md`, language server Phase 3). **Never put unbounded work on the read
  pool.**
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

## Conventions

- Navigate the tree through the typed AST wrappers, not raw `children()`/
  `kind()` matching. The exceptions are the polymorphic kind-classification
  walkers (`symbols.rs`, `folding.rs`, `semantic_tokens.rs`), where a single
  node dispatches over many kinds and single-kind wrappers would add code.
- `latex_symbols.rs` is **generated** by `scripts/generate-latex-symbols.jl`
  from `REPL.REPLCompletions`; regenerate on a Julia bump, do not hand-edit.
  Both tables are sorted by key and `completion.rs` binary-searches them.
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
