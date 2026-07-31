//! Server-initiated work-done progress (`$/progress`) for the workspace
//! harvester's indexing passes.
//!
//! The harvester resolves the environment and parses all of Base off the event
//! loop; on first open that can take seconds during which library navigation
//! looks broken. [`HarvestProgress`] wraps the LSP handshake for reporting it:
//! a `window/workDoneProgress/create` request to mint a token, then
//! `$/progress` `begin`/`report`/`end` notifications the editor renders as a
//! spinner.
//!
//! A no-op when the client did not advertise `window.workDoneProgress`: the
//! `sender` is then `None`, every method returns early, and no token is minted.

use std::cell::Cell;

use crossbeam_channel::Sender;
use lsp_server::{Message, Notification, Request, RequestId};
use lsp_types::notification::{Notification as _, Progress};
use lsp_types::request::{Request as _, WorkDoneProgressCreate};
use lsp_types::{
    NumberOrString, ProgressParams, ProgressParamsValue, ProgressToken, WorkDoneProgress,
    WorkDoneProgressBegin, WorkDoneProgressCreateParams, WorkDoneProgressEnd,
    WorkDoneProgressReport,
};

/// Emits `$/progress` for one harvest at a time. Cheap to construct and to call
/// when disabled. Lives on the (single) harvester thread, so it uses `Cell`
/// rather than atomics.
pub(crate) struct HarvestProgress {
    /// The client's message channel, or `None` when the client lacks
    /// `window.workDoneProgress` support (then every method is a no-op).
    sender: Option<Sender<Message>>,
    /// Next token suffix. Harvests are sequential, but a distinct token per
    /// cycle keeps a late `end` from a prior cycle from colliding with a new
    /// `begin`.
    next: Cell<u64>,
    /// The active cycle's token suffix, set by [`begin`](Self::begin) and
    /// cleared by [`end`](Self::end). `report`/`end` are no-ops without it.
    active: Cell<Option<u64>>,
}

impl HarvestProgress {
    pub(crate) fn new(sender: Option<Sender<Message>>) -> Self {
        Self {
            sender,
            next: Cell::new(0),
            active: Cell::new(None),
        }
    }

    fn token(id: u64) -> ProgressToken {
        NumberOrString::String(format!("fatou/harvest/{id}"))
    }

    /// Mint a token, ask the client to create the progress, and begin it. The
    /// create request is fire-and-forget: the main loop ignores its response,
    /// and sending `create` then `begin` in order from this thread is enough in
    /// practice.
    pub(crate) fn begin(&self, title: &str, message: &str) {
        let Some(sender) = &self.sender else {
            return;
        };
        let id = self.next.get();
        self.next.set(id + 1);
        self.active.set(Some(id));
        let token = Self::token(id);
        let _ = sender.send(Message::Request(Request {
            id: RequestId::from(format!("fatou-progress-{id}")),
            method: WorkDoneProgressCreate::METHOD.to_string(),
            params: serde_json::to_value(WorkDoneProgressCreateParams {
                token: token.clone(),
            })
            .expect("work-done progress create params serialize"),
        }));
        self.send(
            token,
            WorkDoneProgress::Begin(WorkDoneProgressBegin {
                title: title.to_string(),
                message: Some(message.to_string()),
                ..Default::default()
            }),
        );
    }

    /// Update the active progress with a new detail message. No-op if no
    /// [`begin`](Self::begin) is in flight.
    pub(crate) fn report(&self, message: &str) {
        let Some(id) = self.active.get() else {
            return;
        };
        self.send(
            Self::token(id),
            WorkDoneProgress::Report(WorkDoneProgressReport {
                message: Some(message.to_string()),
                ..Default::default()
            }),
        );
    }

    /// End the active progress. No-op if no [`begin`](Self::begin) is in flight.
    pub(crate) fn end(&self, message: &str) {
        let Some(id) = self.active.take() else {
            return;
        };
        self.send(
            Self::token(id),
            WorkDoneProgress::End(WorkDoneProgressEnd {
                message: Some(message.to_string()),
            }),
        );
    }

    fn send(&self, token: ProgressToken, value: WorkDoneProgress) {
        let Some(sender) = &self.sender else {
            return;
        };
        let note = Notification::new(
            Progress::METHOD.to_string(),
            ProgressParams {
                token,
                value: ProgressParamsValue::WorkDone(value),
            },
        );
        let _ = sender.send(Message::Notification(note));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work_done(msg: &Message) -> Option<WorkDoneProgress> {
        let Message::Notification(note) = msg else {
            return None;
        };
        if note.method != Progress::METHOD {
            return None;
        }
        let params: ProgressParams = serde_json::from_value(note.params.clone()).unwrap();
        let ProgressParamsValue::WorkDone(value) = params.value;
        Some(value)
    }

    #[test]
    fn disabled_reporter_sends_nothing() {
        let progress = HarvestProgress::new(None);
        progress.begin("Indexing", "Resolving environment");
        progress.report("Indexing Base and packages");
        progress.end("Indexed 0 packages");
        // Nothing to assert against a `None` sender beyond "it did not panic";
        // the point is that no token is minted and no channel is touched.
        assert_eq!(progress.next.get(), 0);
        assert_eq!(progress.active.get(), None);
    }

    #[test]
    fn emits_create_then_begin_report_end() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let progress = HarvestProgress::new(Some(tx));
        progress.begin("Indexing", "Resolving environment");
        progress.report("Indexing Base and packages");
        progress.end("Indexed 3 packages");

        // First: the create request.
        let create = rx.recv().unwrap();
        let Message::Request(req) = create else {
            panic!("expected a create request, got {create:?}");
        };
        assert_eq!(req.method, WorkDoneProgressCreate::METHOD);
        let params: WorkDoneProgressCreateParams = serde_json::from_value(req.params).unwrap();

        // Then begin/report/end, all carrying the created token.
        let begin = rx.recv().unwrap();
        assert!(matches!(
            work_done(&begin),
            Some(WorkDoneProgress::Begin(_))
        ));
        let Message::Notification(note) = &begin else {
            unreachable!()
        };
        let begin_params: ProgressParams = serde_json::from_value(note.params.clone()).unwrap();
        assert_eq!(begin_params.token, params.token);

        assert!(matches!(
            work_done(&rx.recv().unwrap()),
            Some(WorkDoneProgress::Report(_))
        ));
        assert!(matches!(
            work_done(&rx.recv().unwrap()),
            Some(WorkDoneProgress::End(_))
        ));
    }

    #[test]
    fn report_and_end_without_begin_are_noops() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let progress = HarvestProgress::new(Some(tx));
        progress.report("stray");
        progress.end("stray");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn successive_cycles_use_distinct_tokens() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let progress = HarvestProgress::new(Some(tx));
        progress.begin("Indexing", "a");
        progress.end("a");
        progress.begin("Indexing", "b");
        progress.end("b");

        let token_of = |msg: &Message| -> ProgressToken {
            let Message::Request(req) = msg else {
                panic!("expected create request");
            };
            let params: WorkDoneProgressCreateParams =
                serde_json::from_value(req.params.clone()).unwrap();
            params.token
        };
        let first = token_of(&rx.recv().unwrap());
        let _ = rx.recv().unwrap(); // begin
        let _ = rx.recv().unwrap(); // end
        let second = token_of(&rx.recv().unwrap());
        assert_ne!(first, second);
    }
}
