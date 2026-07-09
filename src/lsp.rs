//! A Julia language server over `lsp-server`'s stdio JSON-RPC transport.
//!
//! Architecture (after arity's dedicated-analysis-thread design, itself modeled
//! on rust-analyzer): the **main loop owns no salsa database**. A dedicated
//! analysis thread ([`analysis_thread`]) owns the persistent
//! [`IncrementalDatabase`](crate::incremental::IncrementalDatabase) and is the
//! sole *writer* — salsa is strictly single-writer. Each analysis is split into
//! a cheap **write-phase** (`&mut db`, on the analysis thread: upsert the live
//! buffer) and a **read-phase** (`&db` only) that runs on the read pool holding
//! a short-lived db clone under `salsa::Cancelled::catch`, so the analysis
//! thread returns to its `select!` immediately and a slow read never blocks
//! queued work.
//!
//! Threading uses a purpose-built [`TaskPool`](task_pool::TaskPool) rather than
//! rayon's global pool (which has no priority concept): the **read pool**,
//! sized to the machine's parallelism, serves latency-sensitive work
//! (formatting, the analysis read-phase). A single-thread **index pool** will
//! join it when background package indexing lands (see `TODO.md`, language
//! server Phase 3) — the one unbounded-duration job must never slot-block a
//! read.
//!
//! Edits are *coalesced* (latest version per URI; stale edits dropped) into a
//! pending queue. A [`decide`](analysis_thread::decide) scheduler keeps at most
//! one analysis in flight: a strictly-newer edit of the *same* URI cancels the
//! running analysis via `salsa::Database::trigger_cancellation` (the worker's
//! `salsa::Cancelled` catch then publishes nothing), while a *different*
//! pending URI waits its turn. Diagnostics route back through the main loop,
//! which drops publishes for closed or superseded documents (a version gate
//! that backstops the rare finish-during-cancel race).
//!
//! Read-only requests reuse the analysis thread's cached work rather than
//! re-parsing: formatting is sent to the analysis thread as a
//! [`ReadJob`](read_jobs::ReadJob); it mints a short-lived db clone and runs
//! the job on the read pool, formatting off the cached parse tree when the
//! tracked buffer still matches the live text. A clone outstanding when the
//! analysis thread writes trips `salsa::Cancelled`; both that and a cache miss
//! fall back to a fresh parse, so reads are always correct, only sometimes
//! warm.

// `lsp_types::Uri` (a `fluent_uri` newtype) carries an internal `Cell` tag for
// its mutable-view mechanism, which trips `clippy::mutable_key_type` when a
// `Uri` is used as a map key. Our URIs are owned + parsed (never "taken"), and
// `Uri`'s `Hash`/`Eq` go through `as_str()`, so this is sound. Allow it
// module-wide.
#![allow(clippy::mutable_key_type)]

mod analysis_thread;
mod completion;
mod folding;
mod format;
mod read_jobs;
mod selection;
mod semantic_tokens;
mod server;
mod state;
mod symbols;
mod task_pool;
mod uri;

pub use completion::compute_completions;
pub use folding::compute_folding_ranges;
pub use format::{compute_format_edits, compute_format_range_edits};
pub use selection::compute_selection_ranges;
pub use semantic_tokens::compute_semantic_tokens;
pub use server::{run, serve};
pub use symbols::compute_document_symbols;
