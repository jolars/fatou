//! Dependency compatibility edits over the live project buffer.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lsp_server::{Message, RequestId, Response};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, Range, TextEdit, Uri,
    WorkspaceEdit,
};

use crate::environment::{Uuid, parse_project_text};
use crate::julia_version::parse_compat;
use crate::registry::{RegistryCache, Releases};
use crate::text::{PositionEncoding, TextBuffer};

use super::read_jobs::ReadReply;
use super::task_pool::TaskPool;

/// Created lazily: only project code actions need to scan registry archives.
/// The single worker keeps those scans off both the main loop and read pool.
pub(crate) struct ProjectActions {
    pool: TaskPool,
    cache: Arc<Mutex<RegistryCache>>,
    depots: Arc<Vec<PathBuf>>,
}

impl ProjectActions {
    pub(crate) fn new(depots: Vec<PathBuf>) -> Self {
        Self {
            pool: TaskPool::new("fatou-registry", 1),
            cache: Arc::default(),
            depots: Arc::new(depots),
        }
    }

    pub(crate) fn request(
        &self,
        id: RequestId,
        params: CodeActionParams,
        text: Arc<TextBuffer>,
        encoding: PositionEncoding,
        reply: ReadReply,
    ) {
        let cache = Arc::clone(&self.cache);
        let depots = Arc::clone(&self.depots);
        self.pool.spawner().spawn(move || {
            let requested = targets(&text, params.range, encoding)
                .into_iter()
                .map(|target| (target.uuid, target.name))
                .collect();
            let versions = cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .latest(&depots, &requested);
            let actions = actions_for(
                &text,
                &params.text_document.uri,
                params.range,
                encoding,
                &versions,
            );
            let _ = reply.send(Message::Response(Response::new_ok(id, actions)));
        });
    }
}

struct Target {
    uuid: Uuid,
    name: String,
    bound: String,
    span: std::ops::Range<usize>,
}

fn targets(text: &TextBuffer, range: Range, encoding: PositionEncoding) -> Vec<Target> {
    let Ok(project) = parse_project_text(Path::new("Project.toml"), text) else {
        return Vec::new();
    };
    let index = text.line_index();
    let start = index.position_to_byte(range.start, encoding);
    let end = index.position_to_byte(range.end, encoding);
    let overlaps = |span: std::ops::Range<usize>| start <= span.end && span.start <= end;
    let mut targets = Vec::new();
    for (name, bound) in &project.compat {
        let mut selected = overlaps(name.span().start..bound.span().end);
        let name = name.as_ref().as_str();
        if name == "julia" || project.sources.contains_key(name) {
            continue;
        }
        let mut uuids = BTreeSet::new();
        for table in [&project.deps, &project.weakdeps, &project.extras] {
            if let Some((key, value)) = table.get_key_value(name) {
                selected |= overlaps(key.span().start..value.span().end);
                let Ok(uuid) = value.as_ref().parse::<Uuid>() else {
                    uuids.clear();
                    break;
                };
                uuids.insert(uuid);
            }
        }
        if selected && uuids.len() == 1 {
            targets.push(Target {
                uuid: *uuids.first().unwrap(),
                name: name.to_owned(),
                bound: bound.as_ref().clone(),
                span: bound.span(),
            });
        }
    }
    targets.sort_by_key(|target| target.span.start);
    targets
}

fn actions_for(
    text: &TextBuffer,
    uri: &Uri,
    range: Range,
    encoding: PositionEncoding,
    versions: &Releases,
) -> Vec<CodeActionOrCommand> {
    let index = text.line_index();
    targets(text, range, encoding)
        .into_iter()
        .filter_map(|target| {
            let (name, version) = versions.get(&target.uuid)?;
            if name != &target.name {
                return None;
            }
            // A local registry may lag the project. Never suggest reducing any
            // explicitly declared minimum, including a later arm of a union.
            let floors: Result<Vec<_>, _> = target.bound.split(',').map(parse_compat).collect();
            if floors.ok()?.iter().any(|bound| bound.min >= *version) {
                return None;
            }
            let edit = TextEdit {
                range: Range::new(
                    index.byte_to_position(target.span.start, encoding),
                    index.byte_to_position(target.span.end, encoding),
                ),
                new_text: format!("\"{version}\""),
            };
            Some(CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("Replace `{name}` compat with `{version}` (local registry)"),
                kind: Some(CodeActionKind::REFACTOR_REWRITE),
                edit: Some(WorkspaceEdit {
                    changes: Some(HashMap::from([(uri.clone(), vec![edit])])),
                    ..Default::default()
                }),
                ..Default::default()
            }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::julia_version::Version;
    use lsp_types::Position;
    use std::collections::BTreeMap;

    const UUID: &str = "12345678-1234-1234-1234-123456789abc";

    #[test]
    fn withholds_updates_for_missing_invalid_current_or_newer_bounds() {
        let uri = super::super::uri::from_path(&std::env::temp_dir().join("Project.toml")).unwrap();
        let versions = BTreeMap::from([(
            UUID.parse().unwrap(),
            ("Example".into(), Version::new(2, 3, 4)),
        )]);
        let range = Range::new(Position::new(0, 0), Position::new(100, 0));
        for text in [
            format!("[deps]\nExample = \"{UUID}\"\n"),
            "[compat]\nExample = \"1\"\n".into(),
            "[deps]\nExample = \"bad uuid\"\n[compat]\nExample = \"1\"\n".into(),
            "[deps]\nExample = [".into(),
            format!("[deps]\nExample = \"{UUID}\"\n[compat]\nExample = \"garbage\"\n"),
            format!("[deps]\nExample = \"{UUID}\"\n[compat]\nExample = \"2.3.4\"\n"),
            format!("[deps]\nExample = \"{UUID}\"\n[compat]\nExample = \"1, 3\"\n"),
            format!(
                "[deps]\nExample = \"{UUID}\"\n[compat]\nExample = \"1\"\n[sources]\nExample = {{path = \"../Example\"}}\n"
            ),
        ] {
            assert!(
                actions_for(
                    &TextBuffer::new(text.clone()),
                    &uri,
                    range,
                    PositionEncoding::Utf16,
                    &versions
                )
                .is_empty(),
                "{text}"
            );
        }
    }

    #[test]
    fn updates_weakdeps_and_extras_using_negotiated_encoding() {
        let uri = super::super::uri::from_path(&std::env::temp_dir().join("Project.toml")).unwrap();
        let versions = BTreeMap::from([(
            UUID.parse().unwrap(),
            ("Δelta".into(), Version::new(2, 3, 4)),
        )]);
        for table in ["weakdeps", "extras"] {
            let text = TextBuffer::new(format!(
                "[{table}]\n\"Δelta\" = \"{UUID}\"\n[compat]\n\"Δelta\" = \"1\" # Comment.\n"
            ));
            for (encoding, start) in [(PositionEncoding::Utf8, 11), (PositionEncoding::Utf16, 10)] {
                let cursor = Range::new(Position::new(3, start), Position::new(3, start));
                let actions = actions_for(&text, &uri, cursor, encoding, &versions);
                let CodeActionOrCommand::CodeAction(action) = &actions[0] else {
                    panic!("expected edit")
                };
                let edits = &action.edit.as_ref().unwrap().changes.as_ref().unwrap()[&uri];
                assert_eq!(
                    edits[0].range,
                    Range::new(Position::new(3, start), Position::new(3, start + 3))
                );
            }
        }
    }

    #[test]
    fn edits_only_the_live_compat_value_and_preserves_comments_and_crlf() {
        let text = TextBuffer::new(format!(
            "[deps]\r\nExample = \"{UUID}\"\r\n[compat]\r\nExample = '1.2' # Keep me.\r\n"
        ));
        let uri = super::super::uri::from_path(&std::env::temp_dir().join("Project.toml")).unwrap();
        let versions = BTreeMap::from([(
            UUID.parse().unwrap(),
            ("Example".into(), Version::new(2, 3, 4)),
        )]);
        for line in [1, 3] {
            let cursor = Range::new(Position::new(line, 2), Position::new(line, 2));
            let actions = actions_for(&text, &uri, cursor, PositionEncoding::Utf16, &versions);
            assert_eq!(actions.len(), 1);
            let CodeActionOrCommand::CodeAction(action) = &actions[0] else {
                panic!("expected an edit");
            };
            let edits = &action.edit.as_ref().unwrap().changes.as_ref().unwrap()[&uri];
            assert_eq!(edits.len(), 1);
            assert_eq!(
                edits[0].range,
                Range::new(Position::new(3, 10), Position::new(3, 15))
            );
            assert_eq!(edits[0].new_text, "\"2.3.4\"");
        }
    }
}
