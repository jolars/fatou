//! Read-only jobs serviced off the analysis thread's cached state.

use std::path::PathBuf;

use crossbeam_channel::Sender;
use lsp_server::{Message, RequestId, Response};

use crate::formatter::FormatStyle;
use crate::incremental::Analysis;
use crate::text::PositionEncoding;

use super::format::format_edits_via_db;

/// A read-only request the analysis thread services by cloning its salsa db
/// and running the work off-thread on the read pool. Each variant carries the
/// live buffer `text` and the client `sender` so the worker can reply
/// directly; the analysis thread only adds the db snapshot. See [`run_read`].
pub(crate) enum ReadJob {
    Format {
        id: RequestId,
        path: PathBuf,
        text: String,
        style: FormatStyle,
        sender: Sender<Message>,
    },
}

impl ReadJob {
    /// Recover the request `id` and reply `sender` from an undeliverable job so
    /// the client still gets a (null) response instead of hanging.
    pub(crate) fn into_reply_parts(self) -> (RequestId, Sender<Message>) {
        match self {
            ReadJob::Format { id, sender, .. } => (id, sender),
        }
    }
}

/// Service a read-only job against a db `snapshot`, replying to the client.
/// Runs on a read-pool worker; the `snapshot` is dropped on return so it never
/// blocks the analysis thread's next write longer than the job itself.
pub(crate) fn run_read(snapshot: Analysis, job: ReadJob, encoding: PositionEncoding) {
    match job {
        ReadJob::Format {
            id,
            path,
            text,
            style,
            sender,
        } => {
            let result = format_edits_via_db(&snapshot, &path, &text, style, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(id, result)));
        }
    }
}
