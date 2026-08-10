//! File and folder rename (`workspace/willRenameFiles`,
//! `workspace/didRenameFiles`).
//!
//! When the user moves a `.jl` file — or a whole folder — in the editor's
//! explorer, every static `include("path")` literal that names a moved file
//! must be re-spelled, in both directions: the files that *include* a moved
//! file, and a moved file's *own* relative includes, which were spelled against
//! the directory it just left. A folder rename is only a path-prefix mapping,
//! so files and folders fall out of one mechanism ([`RenameMap`]).
//!
//! The edits are returned against each file's **old** URI: the LSP applies a
//! `willRenameFiles` edit *before* the client performs the move, so the old
//! path is still the one the db (and the client) knows.
//!
//! Deliberately conservative. A literal is rewritten only when the rename
//! actually moved its target or the directory it is spelled from — a
//! non-canonical spelling of an untouched target (`include("sub/../a.jl")`), or
//! a roundabout escaping of one (`include("\\x61.jl")`), is left exactly as
//! written rather than silently canonicalized. The comparison is between
//! *decoded* paths ([`crate::project::IncludeSite::path`]) and the replacement is re-escaped on
//! the way out, so an escape in the literal is no obstacle.
//!
//! Package *identity* is not repaired: renaming a package's entry file
//! (`src/MyPkg.jl`) rebases its own includes but leaves `Project.toml`'s `name`
//! alone, and the existing include-graph diagnostics surface the breakage.

use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use lsp_types::{FileRename, Range, TextEdit, Uri, WorkspaceEdit};

use crate::incremental::{Analysis, SourceFile, normalize_path};
use crate::project::{include_sites, resolve_target};
use crate::text::{LineIndex, PositionEncoding};

use super::uri;

/// The edits that keep the workspace's `include` graph intact across the rename
/// batch `files`, or `None` when nothing needs rewriting (answered as `null`).
///
/// `open_docs` are the paths of the client's open buffers: the seeded member
/// set covers only what the harvest reached from each package's entry file, so
/// an open `test/runtests.jl` would otherwise be left with a dangling include.
pub(crate) fn will_rename_files_via_db(
    snapshot: &Analysis,
    files: &[FileRename],
    open_docs: &[PathBuf],
    encoding: PositionEncoding,
) -> Option<WorkspaceEdit> {
    let map = RenameMap::from_files(files)?;
    // A write racing the read trips `salsa::Cancelled`. Unlike the per-document
    // handlers there is no client-supplied text to re-parse from, so a race
    // simply answers `null`; the client re-requests on the next rename.
    let changes = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        collect_edits(snapshot, &map, open_docs, encoding)
    }))
    .ok()?;
    (!changes.is_empty()).then(|| WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
    })
}

/// Every file whose `include` literals the batch could disturb, paired with its
/// (normalized, pre-rename) path: the seeded workspace members plus any open
/// buffer the harvest never reached.
fn scan_set(snapshot: &Analysis, open_docs: &[PathBuf]) -> Vec<(SourceFile, PathBuf)> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut scan: Vec<(SourceFile, PathBuf)> = Vec::new();
    let mut push = |file: SourceFile, path: PathBuf| {
        // A synthetic path stands in for a non-`file:` URI (an untitled
        // buffer): it names no directory a relative include could resolve
        // against, and nothing can be renamed there.
        if !uri::is_synthetic(&path) && seen.insert(path.clone()) {
            scan.push((file, path));
        }
    };
    for file in snapshot.workspace_files() {
        if let Some(path) = snapshot.file_path_of(file) {
            push(file, normalize_path(&path));
        }
    }
    for path in open_docs {
        let path = normalize_path(path);
        if let Some(file) = snapshot.lookup_file(&path) {
            push(file, path);
        }
    }
    scan
}

/// Walk the scan set and collect each file's include rewrites under its **old**
/// URI (see the module doc).
fn collect_edits(
    snapshot: &Analysis,
    map: &RenameMap,
    open_docs: &[PathBuf],
    encoding: PositionEncoding,
) -> HashMap<Uri, Vec<TextEdit>> {
    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
    for (file, old_path) in scan_set(snapshot, open_docs) {
        let new_path = map.map(&old_path);
        // The cheap gate, off the cached `include_edges` firewall: a file
        // matters only if it moved (its own relative includes must rebase) or
        // one of its targets moved. `IncludeEdge::target` is a plain join, so
        // it must be normalized before it can match a rename key.
        let disturbed = new_path != old_path
            || snapshot.include_edges(file).iter().any(|edge| {
                edge.target.as_deref().is_some_and(|target| {
                    let target = normalize_path(target);
                    map.map(&target) != target
                })
            });
        if !disturbed {
            continue;
        }
        let (Some(old_dir), Some(new_dir)) = (old_path.parent(), new_path.parent()) else {
            continue;
        };
        let text = snapshot.file_text_of(file);
        let line_index = LineIndex::new(text);
        let edits: Vec<TextEdit> = include_sites(&snapshot.parsed_tree(file))
            .into_iter()
            .filter_map(|site| {
                let content = site.content?;
                let new_text = rewritten_literal(&site.path, old_dir, new_dir, map)?;
                Some(TextEdit {
                    range: Range::new(
                        line_index.byte_to_position(content.start().into(), encoding),
                        line_index.byte_to_position(content.end().into(), encoding),
                    ),
                    new_text,
                })
            })
            .collect();
        insert_edits(&mut changes, &old_path, edits);
    }
    changes
}

/// A resolved rename batch: normalized old path → normalized new path, sorted
/// most-specific-first so a nested entry wins over a folder containing it.
#[derive(Debug, Clone)]
pub(crate) struct RenameMap {
    entries: Vec<(PathBuf, PathBuf)>,
}

impl RenameMap {
    /// The batch's `file:` entries, or `None` when none survive: a non-`file:`
    /// URI names no path on disk, and an entry that maps a path to itself moves
    /// nothing.
    pub(crate) fn from_files(files: &[FileRename]) -> Option<Self> {
        let mut entries: Vec<(PathBuf, PathBuf)> = files
            .iter()
            .filter_map(|rename| {
                let old = path_of(&rename.old_uri)?;
                let new = path_of(&rename.new_uri)?;
                (old != new).then_some((old, new))
            })
            .collect();
        if entries.is_empty() {
            return None;
        }
        // Most specific first: with a (client-improbable) batch holding both a
        // folder and something inside it, the inner entry decides.
        entries.sort_by_key(|(old, _)| std::cmp::Reverse(old.components().count()));
        Some(Self { entries })
    }

    /// Where `path` (already normalized) ends up after the batch. A single
    /// pass, never chained: a batch of `a → b` and `b → c` must not compose
    /// into `a → c`, which is not the move the client described.
    pub(crate) fn map(&self, path: &Path) -> PathBuf {
        for (old, new) in &self.entries {
            if path == old {
                return new.clone();
            }
            if let Ok(rest) = path.strip_prefix(old) {
                return new.join(rest);
            }
        }
        path.to_path_buf()
    }
}

/// The normalized path a `file:` URI string names, or `None` for anything
/// without a filesystem identity (a non-`file:` scheme, an unparsable URI).
fn path_of(text: &str) -> Option<PathBuf> {
    let uri = Uri::from_str(text).ok()?;
    let path = uri::to_path(&uri)?;
    Some(normalize_path(&path))
}

/// The source text the include literal denoting `path` should become once the
/// batch is applied, or `None` to leave the site untouched. `old_dir`/`new_dir`
/// are the *including* file's directory before and after the batch.
fn rewritten_literal(
    path: &str,
    old_dir: &Path,
    new_dir: &Path,
    map: &RenameMap,
) -> Option<String> {
    // An empty literal names nothing.
    if path.is_empty() {
        return None;
    }
    let was_absolute = Path::new(path).is_absolute();
    let dotted = path.starts_with("./");

    let old_target = normalize_path(&resolve_target(path, Some(old_dir))?);
    let new_target = map.map(&old_target);

    // Neither the target nor the directory it is spelled from moved: never
    // re-spell a literal the rename did not touch.
    if new_target == old_target && new_dir == old_dir {
        return None;
    }
    // An absolute literal does not depend on the includer's directory at all.
    if was_absolute && new_target == old_target {
        return None;
    }

    let spelling = if was_absolute {
        absolute_spelling(&new_target)?
    } else {
        let relative =
            relative_spelling(new_dir, &new_target).or_else(|| absolute_spelling(&new_target))?;
        if dotted && !relative.starts_with("../") {
            format!("./{relative}")
        } else {
            relative
        }
    };
    // Compare decoded against decoded: a literal that already denotes the new
    // path stays as written, however it spells it.
    (spelling != path).then(|| escape_literal(&spelling))
}

/// `target` spelled relative to `from_dir`, with `/` separators (Julia accepts
/// them on every platform). Both paths must already be normalized, so their
/// components are canonical. `None` when they share no component at all (a
/// different Windows drive or UNC prefix), or when `target` *is* `from_dir`.
fn relative_spelling(from_dir: &Path, target: &Path) -> Option<String> {
    let from: Vec<Component<'_>> = from_dir.components().collect();
    let to: Vec<Component<'_>> = target.components().collect();
    let common = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    if common == 0 {
        return None;
    }
    let mut parts: Vec<&str> = vec![".."; from.len() - common];
    for component in &to[common..] {
        parts.push(component.as_os_str().to_str()?);
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// `path` spelled as an absolute literal, with `/` separators.
fn absolute_spelling(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    // Only Windows spells separators with a backslash; on Unix a backslash is
    // an ordinary filename character and must survive into the escaping below.
    #[cfg(windows)]
    let text = text.replace('\\', "/");
    Some(text.to_string())
}

/// Escape `path` for a plain double-quoted Julia string literal.
fn escape_literal(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for ch in path.chars() {
        if matches!(ch, '\\' | '"' | '$') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Collect `edits` into `changes` under `path`'s URI, sorted by position and
/// deduplicated. Content spans within one file are disjoint and already in
/// source order; the pass mirrors [`rename`](super::rename) and guarantees the
/// no-overlap rule a [`WorkspaceEdit`] must satisfy.
fn insert_edits(changes: &mut HashMap<Uri, Vec<TextEdit>>, path: &Path, mut edits: Vec<TextEdit>) {
    if edits.is_empty() {
        return;
    }
    let Some(uri) = uri::from_path(path) else {
        return;
    };
    edits.sort_by_key(|edit| (edit.range.start.line, edit.range.start.character));
    edits.dedup_by_key(|edit| (edit.range.start.line, edit.range.start.character));
    changes.entry(uri).or_default().extend(edits);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A platform-native absolute path. Unix-style `/work` is *not* absolute on
    /// Windows, so prefix a drive there. Forward slashes, so the result can be
    /// embedded in Julia source literals.
    fn abs(path: &str) -> String {
        if cfg!(windows) {
            format!("C:{path}")
        } else {
            path.to_string()
        }
    }

    /// The `file:` URI for [`abs`]`(path)`.
    fn file_uri(path: &str) -> String {
        if cfg!(windows) {
            format!("file:///C:{path}")
        } else {
            format!("file://{path}")
        }
    }

    fn rename(old: &str, new: &str) -> FileRename {
        FileRename {
            old_uri: file_uri(old),
            new_uri: file_uri(new),
        }
    }

    fn map_of(renames: &[FileRename]) -> RenameMap {
        RenameMap::from_files(renames).expect("a non-empty rename map")
    }

    /// The literal denoting `path` rewritten for an includer sitting at
    /// `includer`. Returns the replacement *source* text (escapes and all).
    fn rewrite(path: &str, includer: &str, renames: &[FileRename]) -> Option<String> {
        let map = map_of(renames);
        let old_path = normalize_path(Path::new(&abs(includer)));
        let new_path = map.map(&old_path);
        rewritten_literal(
            path,
            old_path.parent().unwrap(),
            new_path.parent().unwrap(),
            &map,
        )
    }

    #[test]
    fn maps_a_file_rename_and_leaves_other_paths_alone() {
        let map = map_of(&[rename("/work/src/a.jl", "/work/src/b.jl")]);
        assert_eq!(
            map.map(Path::new(&abs("/work/src/a.jl"))),
            PathBuf::from(abs("/work/src/b.jl"))
        );
        assert_eq!(
            map.map(Path::new(&abs("/work/src/c.jl"))),
            PathBuf::from(abs("/work/src/c.jl"))
        );
    }

    #[test]
    fn maps_a_folder_rename_as_a_path_prefix() {
        let map = map_of(&[rename("/work/src/sub", "/work/src/nested")]);
        assert_eq!(
            map.map(Path::new(&abs("/work/src/sub/deep/a.jl"))),
            PathBuf::from(abs("/work/src/nested/deep/a.jl"))
        );
        // A sibling whose name merely starts with the same text is untouched:
        // `starts_with` compares whole components.
        assert_eq!(
            map.map(Path::new(&abs("/work/src/submarine.jl"))),
            PathBuf::from(abs("/work/src/submarine.jl"))
        );
    }

    #[test]
    fn the_most_specific_entry_wins_and_mappings_never_chain() {
        let map = map_of(&[
            rename("/work/src", "/work/lib"),
            rename("/work/src/a.jl", "/work/src/keep.jl"),
        ]);
        // The inner entry decides for the file it names...
        assert_eq!(
            map.map(Path::new(&abs("/work/src/a.jl"))),
            PathBuf::from(abs("/work/src/keep.jl")),
            "the nested entry wins, and its result is not re-mapped by the folder entry"
        );
        // ...and the folder entry still covers everything else under it.
        assert_eq!(
            map.map(Path::new(&abs("/work/src/b.jl"))),
            PathBuf::from(abs("/work/lib/b.jl"))
        );
    }

    #[test]
    fn a_non_file_uri_entry_is_dropped_from_the_map() {
        let only_untitled = [FileRename {
            old_uri: "untitled:Untitled-1".to_string(),
            new_uri: "untitled:Untitled-2".to_string(),
        }];
        assert!(RenameMap::from_files(&only_untitled).is_none());
        let identity = [rename("/work/a.jl", "/work/a.jl")];
        assert!(RenameMap::from_files(&identity).is_none());
        assert!(RenameMap::from_files(&[]).is_none());
    }

    #[test]
    fn rewrites_the_includers_literal_when_the_included_file_moves() {
        let edit = rewrite(
            "a.jl",
            "/work/src/MyPkg.jl",
            &[rename("/work/src/a.jl", "/work/src/sub/a.jl")],
        );
        assert_eq!(edit.as_deref(), Some("sub/a.jl"));
    }

    #[test]
    fn a_relative_spelling_walks_up_with_dot_dot() {
        let edit = rewrite(
            "a.jl",
            "/work/src/MyPkg.jl",
            &[rename("/work/src/a.jl", "/work/lib/a.jl")],
        );
        assert_eq!(edit.as_deref(), Some("../lib/a.jl"));
    }

    #[test]
    fn rebases_a_moved_files_own_relative_includes() {
        // `src/a.jl` includes `b.jl` beside it, then moves one level down.
        let edit = rewrite(
            "b.jl",
            "/work/src/a.jl",
            &[rename("/work/src/a.jl", "/work/src/sub/a.jl")],
        );
        assert_eq!(edit.as_deref(), Some("../b.jl"));
    }

    #[test]
    fn a_move_that_carries_both_ends_leaves_the_literal_alone() {
        let edit = rewrite(
            "b.jl",
            "/work/src/sub/a.jl",
            &[rename("/work/src/sub", "/work/src/nested")],
        );
        assert_eq!(edit, None, "the two files stay side by side");
    }

    #[test]
    fn an_absolute_literal_stays_absolute() {
        let edit = rewrite(
            &abs("/work/src/a.jl"),
            "/work/src/MyPkg.jl",
            &[rename("/work/src/a.jl", "/work/src/sub/a.jl")],
        );
        assert_eq!(edit.as_deref(), Some(abs("/work/src/sub/a.jl").as_str()));
    }

    #[test]
    fn an_absolute_literal_is_untouched_when_only_the_includer_moves() {
        let edit = rewrite(
            &abs("/work/src/a.jl"),
            "/work/src/MyPkg.jl",
            &[rename("/work/src/MyPkg.jl", "/work/src/sub/MyPkg.jl")],
        );
        assert_eq!(edit, None, "an absolute literal ignores the includer's dir");
    }

    #[test]
    fn a_leading_dot_slash_is_preserved() {
        let edit = rewrite(
            "./a.jl",
            "/work/src/MyPkg.jl",
            &[rename("/work/src/a.jl", "/work/src/sub/a.jl")],
        );
        assert_eq!(edit.as_deref(), Some("./sub/a.jl"));
    }

    #[test]
    fn a_leading_dot_slash_gives_way_to_a_walk_up() {
        let edit = rewrite(
            "./a.jl",
            "/work/src/MyPkg.jl",
            &[rename("/work/src/a.jl", "/work/lib/a.jl")],
        );
        assert_eq!(edit.as_deref(), Some("../lib/a.jl"));
    }

    #[test]
    fn separators_are_always_forward_slashes() {
        let edit = rewrite(
            "a.jl",
            "/work/src/MyPkg.jl",
            &[rename("/work/src/a.jl", "/work/src/one/two/a.jl")],
        )
        .expect("an edit");
        assert!(!edit.contains('\\'), "got {edit}");
        assert_eq!(edit, "one/two/a.jl");
    }

    #[test]
    fn an_unmoved_include_is_untouched_even_when_spelled_non_canonically() {
        let edit = rewrite(
            "sub/../a.jl",
            "/work/src/MyPkg.jl",
            &[rename("/work/src/other.jl", "/work/src/renamed.jl")],
        );
        assert_eq!(
            edit, None,
            "a literal the rename never touched stays verbatim"
        );
    }

    /// The decode's other half: what comes back out is source text again, so a
    /// path holding a character the literal must escape is re-escaped. Unix
    /// only — a backslash is an ordinary filename character there, while on
    /// Windows it is a separator and can never reach the name.
    #[test]
    #[cfg(unix)]
    fn a_backslash_in_a_name_is_re_escaped_on_the_way_out() {
        let map = RenameMap::from_files(&[FileRename {
            old_uri: "file:///work/src/a.jl".to_string(),
            new_uri: "file:///work/src/o%5Cd.jl".to_string(),
        }])
        .expect("a rename map");
        let edit = rewritten_literal("a.jl", Path::new("/work/src"), Path::new("/work/src"), &map);
        assert_eq!(edit.as_deref(), Some("o\\\\d.jl"));
    }

    #[test]
    fn a_new_path_needing_escapes_is_escaped() {
        let edit = rewrite(
            "a.jl",
            "/work/src/MyPkg.jl",
            &[rename("/work/src/a.jl", "/work/src/od$d/a.jl")],
        );
        assert_eq!(edit.as_deref(), Some("od\\$d/a.jl"));
    }

    #[test]
    #[cfg(windows)]
    fn a_target_on_another_drive_falls_back_to_an_absolute_literal() {
        let map = RenameMap::from_files(&[FileRename {
            old_uri: "file:///C:/work/src/a.jl".to_string(),
            new_uri: "file:///D:/elsewhere/a.jl".to_string(),
        }])
        .expect("a rename map");
        let edit = rewritten_literal(
            "a.jl",
            Path::new("C:\\work\\src"),
            Path::new("C:\\work\\src"),
            &map,
        );
        assert_eq!(edit.as_deref(), Some("D:/elsewhere/a.jl"));
    }
}

/// The scan over a seeded workspace database.
#[cfg(test)]
mod db_tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::incremental::IncrementalDatabase;
    use crate::lsp::cross_file::test_support::workspace_db;

    /// The absolute path of `rel` under the fixture package root `/work/MyPkg`
    /// (normalized, so it is drive-absolute on Windows too).
    fn pkg_path(rel: &str) -> PathBuf {
        normalize_path(&PathBuf::from("/work/MyPkg").join(rel))
    }

    fn uri_of(path: &Path) -> String {
        uri::from_path(path)
            .expect("a file URI")
            .as_str()
            .to_string()
    }

    /// A rename of `old` to `new`, both package-relative.
    fn moved(old: &str, new: &str) -> FileRename {
        FileRename {
            old_uri: uri_of(&pkg_path(old)),
            new_uri: uri_of(&pkg_path(new)),
        }
    }

    /// The rewrites for `renames` over a workspace whose members are `files`
    /// (package-relative under `src/`), as `uri -> [(line, character, new text)]`.
    fn edits(
        files: &[(&str, &str)],
        renames: &[FileRename],
    ) -> BTreeMap<String, Vec<(u32, u32, String)>> {
        let (db, _) = workspace_db(&[], files);
        edits_with(&db, renames, &[])
    }

    /// [`edits`] against a database built by the caller, with `open_docs`.
    fn edits_with(
        db: &IncrementalDatabase,
        renames: &[FileRename],
        open_docs: &[PathBuf],
    ) -> BTreeMap<String, Vec<(u32, u32, String)>> {
        let edit =
            will_rename_files_via_db(&db.snapshot(), renames, open_docs, PositionEncoding::Utf16);
        let Some(edit) = edit else {
            return BTreeMap::new();
        };
        edit.changes
            .expect("changes")
            .into_iter()
            .map(|(uri, edits)| {
                (
                    uri.as_str().to_string(),
                    edits
                        .into_iter()
                        .map(|e| (e.range.start.line, e.range.start.character, e.new_text))
                        .collect(),
                )
            })
            .collect()
    }

    #[test]
    fn rewrites_the_includers_literal_when_the_included_file_moves() {
        let changes = edits(
            &[("MyPkg.jl", "include(\"a.jl\")\n"), ("a.jl", "x = 1\n")],
            &[moved("src/a.jl", "src/sub/a.jl")],
        );
        assert_eq!(
            changes,
            BTreeMap::from([(
                uri_of(&pkg_path("src/MyPkg.jl")),
                vec![(0, 9, "sub/a.jl".to_string())],
            )])
        );
    }

    #[test]
    fn edits_address_the_files_old_uri() {
        // The edited file is itself the one moving, so its edit must land on
        // the path the client (and the db) knows it by *before* the move: a
        // `willRenameFiles` edit is applied before the client performs it.
        let changes = edits(
            &[("a.jl", "include(\"b.jl\")\n"), ("b.jl", "x = 1\n")],
            &[moved("src/a.jl", "src/sub/a.jl")],
        );
        assert!(
            changes.contains_key(&uri_of(&pkg_path("src/a.jl"))),
            "expected the old URI, got {:?}",
            changes.keys().collect::<Vec<_>>()
        );
        assert!(!changes.contains_key(&uri_of(&pkg_path("src/sub/a.jl"))));
    }

    #[test]
    fn rebases_a_moved_files_own_relative_includes() {
        let changes = edits(
            &[("a.jl", "include(\"b.jl\")\n"), ("b.jl", "x = 1\n")],
            &[moved("src/a.jl", "src/sub/a.jl")],
        );
        assert_eq!(
            changes,
            BTreeMap::from([(
                uri_of(&pkg_path("src/a.jl")),
                vec![(0, 9, "../b.jl".to_string())],
            )])
        );
    }

    #[test]
    fn a_folder_rename_that_moves_both_ends_produces_no_edit() {
        let changes = edits(
            &[("sub/a.jl", "include(\"b.jl\")\n"), ("sub/b.jl", "x = 1\n")],
            &[moved("src/sub", "src/nested")],
        );
        assert!(changes.is_empty(), "got {changes:?}");
    }

    #[test]
    fn two_identical_literals_in_one_file_each_get_their_own_edit() {
        let changes = edits(
            &[
                ("MyPkg.jl", "include(\"a.jl\")\ninclude(\"a.jl\")\n"),
                ("a.jl", "x = 1\n"),
            ],
            &[moved("src/a.jl", "src/sub/a.jl")],
        );
        assert_eq!(
            changes[&uri_of(&pkg_path("src/MyPkg.jl"))],
            vec![
                (0, 9, "sub/a.jl".to_string()),
                (1, 9, "sub/a.jl".to_string()),
            ]
        );
    }

    #[test]
    fn an_include_spelled_with_dot_dot_is_matched_after_normalization() {
        let changes = edits(
            &[("sub/a.jl", "include(\"../b.jl\")\n"), ("b.jl", "x = 1\n")],
            &[moved("src/b.jl", "src/c.jl")],
        );
        assert_eq!(
            changes[&uri_of(&pkg_path("src/sub/a.jl"))],
            vec![(0, 9, "../c.jl".to_string())]
        );
    }

    #[test]
    fn one_batch_rewrites_several_files() {
        let changes = edits(
            &[
                ("MyPkg.jl", "include(\"a.jl\")\ninclude(\"b.jl\")\n"),
                ("a.jl", "x = 1\n"),
                ("b.jl", "y = 2\n"),
            ],
            &[
                moved("src/a.jl", "src/sub/a.jl"),
                moved("src/b.jl", "src/sub/b.jl"),
            ],
        );
        assert_eq!(
            changes[&uri_of(&pkg_path("src/MyPkg.jl"))],
            vec![
                (0, 9, "sub/a.jl".to_string()),
                (1, 9, "sub/b.jl".to_string()),
            ]
        );
    }

    #[test]
    fn renaming_the_package_entry_file_still_rebases_its_own_includes() {
        let changes = edits(
            &[("MyPkg.jl", "include(\"a.jl\")\n"), ("a.jl", "x = 1\n")],
            &[moved("src/MyPkg.jl", "src/deep/MyPkg.jl")],
        );
        assert_eq!(
            changes[&uri_of(&pkg_path("src/MyPkg.jl"))],
            vec![(0, 9, "../a.jl".to_string())]
        );
    }

    #[test]
    fn a_rename_outside_the_workspace_yields_no_edit() {
        let changes = edits(
            &[("MyPkg.jl", "include(\"a.jl\")\n"), ("a.jl", "x = 1\n")],
            &[FileRename {
                old_uri: uri_of(&normalize_path(Path::new("/elsewhere/z.jl"))),
                new_uri: uri_of(&normalize_path(Path::new("/elsewhere/y.jl"))),
            }],
        );
        assert!(changes.is_empty(), "got {changes:?}");
    }

    #[test]
    fn an_open_non_member_buffer_is_scanned_too() {
        // `test/runtests.jl` is not reachable from the package entry, so the
        // harvest never made it a member; it is only open in the editor.
        let (mut db, _) = workspace_db(&[], &[("a.jl", "x = 1\n")]);
        let runtests = pkg_path("test/runtests.jl");
        db.upsert_file(&runtests, "include(\"../src/a.jl\")\n".to_string());

        let changes = edits_with(
            &db,
            &[moved("src/a.jl", "src/sub/a.jl")],
            std::slice::from_ref(&runtests),
        );
        assert_eq!(
            changes[&uri_of(&runtests)],
            vec![(0, 9, "../src/sub/a.jl".to_string())]
        );
    }
}
