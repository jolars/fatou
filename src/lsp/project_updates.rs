//! Dependency updates and registry version hints over the live project buffer.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lsp_server::{Message, RequestId, Response};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, InlayHint, InlayHintLabel,
    InlayHintTooltip, Position, Range, TextEdit, Uri, WorkspaceEdit,
};

use crate::environment::{Uuid, parse_project_text};
use crate::julia_version::{CompatSpec, Version};
use crate::registry::{RegistryCache, Releases};
use crate::text::{PositionEncoding, TextBuffer};

use super::read_jobs::ReadReply;
use super::task_pool::TaskPool;

/// Created lazily: only project updates and hints need registry archives.
/// The single worker keeps those scans off both the main loop and read pool.
#[derive(Clone)]
pub(crate) struct ProjectUpdates {
    pool: Arc<TaskPool>,
    cache: Arc<Mutex<RegistryCache>>,
    depots: Arc<Vec<PathBuf>>,
}

impl ProjectUpdates {
    pub(crate) fn new(depots: Vec<PathBuf>) -> Self {
        Self {
            pool: Arc::new(TaskPool::new("fatou-registry", 1)),
            cache: Arc::default(),
            depots: Arc::new(depots),
        }
    }

    pub(crate) fn code_actions(
        &self,
        id: RequestId,
        params: CodeActionParams,
        text: Arc<TextBuffer>,
        encoding: PositionEncoding,
        reply: ReadReply,
    ) {
        self.with_updates(text, params.range, encoding, move |updates| {
            let actions = actions_from_updates(updates, &params.text_document.uri);
            let _ = reply.send(Message::Response(Response::new_ok(id, actions)));
        });
    }

    pub(crate) fn inlay_hints(
        &self,
        id: RequestId,
        text: Arc<TextBuffer>,
        range: Range,
        encoding: PositionEncoding,
        mut resolved: Vec<InlayHint>,
        reply: ReadReply,
    ) {
        self.with_updates(text, range, encoding, move |updates| {
            resolved.extend(hints_from_updates(updates, range));
            resolved.sort_by_key(|hint| hint.position);
            let _ = reply.send(Message::Response(Response::new_ok(id, resolved)));
        });
    }

    fn with_updates(
        &self,
        text: Arc<TextBuffer>,
        range: Range,
        encoding: PositionEncoding,
        finish: impl FnOnce(Vec<DependencyUpdate>) + Send + 'static,
    ) {
        let cache = Arc::clone(&self.cache);
        let depots = Arc::clone(&self.depots);
        self.pool.spawner().spawn(move || {
            let targets = targets(&text, range, encoding);
            let requested = targets
                .iter()
                .map(|target| (target.uuid, target.name.clone()))
                .collect();
            let versions = cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .releases(&depots, &requested);
            finish(dependency_updates(&text, targets, encoding, &versions));
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

struct DependencyUpdate {
    name: String,
    position: Position,
    matching: Option<Version>,
    latest: Version,
    update: Option<TextEdit>,
    upgrade: Option<TextEdit>,
}

fn dependency_updates(
    text: &TextBuffer,
    targets: Vec<Target>,
    encoding: PositionEncoding,
    versions: &Releases,
) -> Vec<DependencyUpdate> {
    let index = text.line_index();
    targets
        .into_iter()
        .filter_map(|target| {
            let (name, versions) = versions.get(&target.uuid)?;
            if name != &target.name {
                return None;
            }
            let spec = CompatSpec::parse(&target.bound).ok()?;
            let latest = *versions.last()?;
            let matching = versions
                .iter()
                .rev()
                .copied()
                .find(|version| spec.contains(*version));
            let edit = |version| {
                let replacement = spec.with_version(version)?;
                if replacement == target.bound {
                    return None;
                }
                let raw = &text[target.span.clone()];
                let quote = if raw.starts_with("\"\"\"") {
                    "\"\"\""
                } else if raw.starts_with("'''") {
                    "'''"
                } else if raw.starts_with('\'') {
                    "'"
                } else {
                    "\""
                };
                Some(TextEdit {
                    range: Range::new(
                        index.byte_to_position(target.span.start, encoding),
                        index.byte_to_position(target.span.end, encoding),
                    ),
                    new_text: format!("{quote}{replacement}{quote}"),
                })
            };
            Some(DependencyUpdate {
                name: name.clone(),
                position: index.byte_to_position(target.span.end, encoding),
                matching,
                latest,
                update: matching.and_then(edit),
                upgrade: (!spec.contains(latest)).then(|| edit(latest)).flatten(),
            })
        })
        .collect()
}

fn actions_from_updates(updates: Vec<DependencyUpdate>, uri: &Uri) -> Vec<CodeActionOrCommand> {
    updates
        .into_iter()
        .flat_map(|info| {
            [("Update", info.update), ("Upgrade", info.upgrade)]
                .into_iter()
                .filter_map(move |(kind, edit)| {
                    let edit = edit?;
                    Some(CodeActionOrCommand::CodeAction(CodeAction {
                        title: format!("{kind} `{}` compat to {}", info.name, edit.new_text),
                        kind: Some(CodeActionKind::REFACTOR_REWRITE),
                        edit: Some(WorkspaceEdit {
                            changes: Some(HashMap::from([(uri.clone(), vec![edit])])),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }))
                })
        })
        .collect()
}

fn hints_from_updates(updates: Vec<DependencyUpdate>, range: Range) -> Vec<InlayHint> {
    updates
        .into_iter()
        .filter(|info| range.start <= info.position && info.position <= range.end)
        .map(version_hint)
        .collect()
}

fn version_hint(info: DependencyUpdate) -> InlayHint {
    let mut label = info.matching.map_or_else(
        || "no matching release".into(),
        |version| format!("v{version}"),
    );
    let mut tooltip = info.matching.map_or_else(
        || {
            format!(
                "No stable release of {} in the local registries matches this compat bound.",
                info.name
            )
        },
        |version| {
            format!(
                "Newest stable release of {} allowed by this compat bound: v{version}.",
                info.name
            )
        },
    );
    if info.upgrade.is_some() {
        label.push_str(&format!(" → v{}", info.latest));
        tooltip.push_str(&format!("\n\nv{} is available outside the current bound; use the Upgrade code action to allow it.", info.latest));
    }
    if info.update.is_some() {
        tooltip.push_str("\n\nThe Update code action adjusts the bound within its existing range.");
    }
    tooltip.push_str("\n\nBased on local registry data, not the installed version. Compatibility with Julia and other dependencies has not been resolved.");
    InlayHint {
        position: info.position,
        label: InlayHintLabel::String(label),
        tooltip: Some(InlayHintTooltip::String(tooltip)),
        padding_left: Some(true),
        padding_right: None,
        kind: None,
        text_edits: None,
        data: None,
    }
}

#[cfg(test)]
fn actions_for(
    text: &TextBuffer,
    uri: &Uri,
    range: Range,
    encoding: PositionEncoding,
    versions: &Releases,
) -> Vec<CodeActionOrCommand> {
    actions_from_updates(
        dependency_updates(text, targets(text, range, encoding), encoding, versions),
        uri,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::julia_version::Version;
    use lsp_types::Position;
    use std::collections::BTreeMap;

    const UUID: &str = "12345678-1234-1234-1234-123456789abc";

    #[test]
    fn actions_and_hints_share_range_aware_version_selection() {
        let uri = super::super::uri::from_path(&std::env::temp_dir().join("Project.toml")).unwrap();
        let versions = BTreeMap::from([(
            UUID.parse().unwrap(),
            (
                "Example".into(),
                ["0.2.9", "1.9.3", "2.3.4"]
                    .map(|version| version.parse().unwrap())
                    .into(),
            ),
        )]);
        let range = Range::new(Position::new(0, 0), Position::new(100, 0));
        for (bound, expected_edits, label) in [
            (
                "1.2",
                vec![("Update", "1.9"), ("Upgrade", "2.3")],
                "v1.9.3 → v2.3.4",
            ),
            ("1", vec![("Upgrade", "2")], "v1.9.3 → v2.3.4"),
            ("2", vec![], "v2.3.4"),
            (
                "~1.2",
                vec![("Upgrade", "~2.3")],
                "no matching release → v2.3.4",
            ),
            ("0.2, 1", vec![("Upgrade", "0.2, 1, 2")], "v1.9.3 → v2.3.4"),
            ("0.2, 2.4", vec![], "v0.2.9"),
            (">= 1.2", vec![], "v2.3.4"),
        ] {
            let text = TextBuffer::new(format!(
                "[deps]\nExample = \"{UUID}\"\n[compat]\nExample = \"{bound}\"\n"
            ));
            let infos = || {
                dependency_updates(
                    &text,
                    targets(&text, range, PositionEncoding::Utf16),
                    PositionEncoding::Utf16,
                    &versions,
                )
            };
            let actions = actions_from_updates(infos(), &uri);
            let actual: Vec<_> = actions
                .iter()
                .map(|action| {
                    let CodeActionOrCommand::CodeAction(action) = action else {
                        panic!("expected edit")
                    };
                    (
                        action.title.clone(),
                        action.edit.as_ref().unwrap().changes.as_ref().unwrap()[&uri][0]
                            .new_text
                            .clone(),
                    )
                })
                .collect();
            let expected: Vec<_> = expected_edits
                .iter()
                .map(|(kind, bound)| {
                    (
                        format!("{kind} `Example` compat to \"{bound}\""),
                        format!("\"{bound}\""),
                    )
                })
                .collect();
            assert_eq!(actual, expected, "{bound}");
            let hints = hints_from_updates(infos(), range);
            assert_eq!(hints.len(), 1, "{bound}");
            let InlayHintLabel::String(actual_label) = &hints[0].label else {
                panic!("expected string label")
            };
            assert_eq!(actual_label, label, "{bound}");
            assert_eq!(hints[0].position, Position::new(3, 12 + bound.len() as u32));
            // The UUID line selects an action, but does not repeat the compat hint.
            assert!(
                hints_from_updates(
                    infos(),
                    Range::new(Position::new(1, 0), Position::new(2, 0))
                )
                .is_empty()
            );
        }
    }

    #[test]
    fn withholds_updates_for_missing_invalid_current_or_newer_bounds() {
        let uri = super::super::uri::from_path(&std::env::temp_dir().join("Project.toml")).unwrap();
        let versions = BTreeMap::from([(
            UUID.parse().unwrap(),
            ("Example".into(), BTreeSet::from([Version::new(2, 3, 4)])),
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
            ("Δelta".into(), BTreeSet::from([Version::new(2, 3, 4)])),
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
            ("Example".into(), BTreeSet::from([Version::new(2, 3, 4)])),
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
            assert_eq!(edits[0].new_text, "'2.3'");
        }
    }
}
