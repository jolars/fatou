//! Navigation over an open project file: the language features a
//! `Project.toml` answers, as opposed to the diagnostics it carries
//! (`environment_diagnostics`).
//!
//! Everything here anchors on a dependency name — [`crate::project_files::dep_at`]
//! decides what the cursor is on — and resolves it through the harvested
//! library, which already knows where each package's source lives. That is the
//! whole feature: a `[deps]` entry is a package name, and the server has a map
//! from package names to source roots.
//!
//! **There is no `compute_*`/`*_via_db` split here**, unlike every Julia
//! feature. Those exist because a Julia handler can serve a *cached parse tree*
//! and must fall back when the tracked input lags the live buffer. The parse
//! here is a TOML parse of the buffer itself, so there is no cache to miss and
//! nothing to be stale against; the database is consulted only for the library
//! map, which is HIGH durability and always current. What remains is the
//! `salsa::Cancelled` guard, since a write may still race the read.
//!
//! A `Manifest.toml` answers none of this. It is not written to the database at
//! all (nothing reads one without an environment resolve), and its `path`
//! entries carry no spans — the manifest is deliberately parsed against a plain
//! table, since its 1.0 and 2.0 layouts differ.

use std::panic::AssertUnwindSafe;
use std::path::PathBuf;

use lsp_types::{
    DocumentLink, Hover, HoverContents, Location, MarkupContent, MarkupKind, Position, Range,
};
use rowan::TextRange;

use crate::environment::{PackageKind, PackageMeta};
use crate::incremental::{Analysis, normalize_path};
use crate::index::Span;
use crate::project_files::{dep_at, dep_entries};
use crate::resolve::PackageSource;
use crate::text::{LineIndex, PositionEncoding, TextBuffer};

use super::definition::site_locations;
use super::uri;

/// What a project file's features need to know about a dependency: where its
/// source is, and what the manifest pinned for it.
///
/// The second half is not on [`PackageSource`] because that trait is
/// resolution's contract — the masking order every consumer of a *name* shares
/// — and a version has no part in resolving one. This is the project file's own
/// view, so it lives with the project file's own features.
pub(crate) trait ProjectLibrary: PackageSource {
    fn package_meta(&self, name: &str) -> Option<PackageMeta>;
}

impl ProjectLibrary for Analysis {
    fn package_meta(&self, name: &str) -> Option<PackageMeta> {
        Analysis::package_meta(self, name)
    }
}

/// Where package `name`'s source begins: its entry file, and the range of the
/// `module <Name>` token inside it.
///
/// The harvester already recorded both — the root module's `DefLocation` is
/// package-root-relative, and `package_root` is what turns it absolute — so
/// this needs no guess at `src/<Name>.jl`, and it lands on the workspace's own
/// dev package as readily as on a depot one.
///
/// `None` for a package the server never harvested: a standard library with no
/// located Julia install, or a registered one whose depot slug is missing. The
/// feature then answers nothing rather than a path that does not exist.
fn package_entry<P: PackageSource>(packages: &P, name: &str) -> Option<(PathBuf, Span)> {
    let root = packages.package_root(name)?;
    let package = packages.package(name)?;
    // Normalized here rather than left to `site_locations`, which does the same
    // for the same reason: document links build their URI straight from this
    // path, and the two features must name a file identically.
    let entry = normalize_path(&root.join(&package.root.loc.file));
    Some((entry, package.root.loc.range))
}

/// Go-to-definition in a project file: a dependency name jumps to its package's
/// entry file. Any other position answers nothing.
pub(crate) fn project_definition<P: PackageSource>(
    text: &TextBuffer,
    position: Position,
    encoding: PositionEncoding,
    packages: &P,
) -> Vec<Location> {
    let offset = text.line_index().position_to_byte(position, encoding);
    let Some(dep) = dep_at(text, offset) else {
        return Vec::new();
    };
    let Some(site) = package_entry(packages, &dep.name) else {
        return Vec::new();
    };
    site_locations(vec![site], encoding)
}

/// [`project_definition`] against a database snapshot. A write racing the read
/// trips `salsa::Cancelled`, which answers nothing — there is no cheaper
/// fallback to take, since the library map is the only thing consulted.
pub(crate) fn project_definition_via_db(
    snapshot: &Analysis,
    text: &TextBuffer,
    position: Position,
    encoding: PositionEncoding,
) -> Vec<Location> {
    salsa::Cancelled::catch(AssertUnwindSafe(|| {
        project_definition(text, position, encoding, snapshot)
    }))
    .unwrap_or_default()
}

/// Hover in a project file: a dependency name reports what the environment
/// resolved it to. Any other position answers nothing.
pub(crate) fn project_hover<L: ProjectLibrary>(
    text: &TextBuffer,
    position: Position,
    encoding: PositionEncoding,
    library: &L,
) -> Option<Hover> {
    let line_index = text.line_index();
    let offset = line_index.position_to_byte(position, encoding);
    let dep = dep_at(text, offset)?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: dep_markdown(&dep.name, library)?,
        }),
        range: Some(to_range(dep.name_range, &line_index, encoding)),
    })
}

/// [`project_hover`] against a database snapshot.
pub(crate) fn project_hover_via_db(
    snapshot: &Analysis,
    text: &TextBuffer,
    position: Position,
    encoding: PositionEncoding,
) -> Option<Hover> {
    salsa::Cancelled::catch(AssertUnwindSafe(|| {
        project_hover(text, position, encoding, snapshot)
    }))
    .unwrap_or_default()
}

/// What is known about the dependency `name`: its version, how it was pinned,
/// and where its source landed. Each line is dropped when unknown, and a name
/// nothing is known about hovers to nothing at all — that it is missing from
/// the manifest is `missing-from-manifest`'s report to make, not this one's.
fn dep_markdown<L: ProjectLibrary>(name: &str, library: &L) -> Option<String> {
    let meta = library.package_meta(name);
    let root = library.package_root(name);
    if meta.is_none() && root.is_none() {
        return None;
    }
    let mut out = format!("**{name}**");
    if let Some(version) = meta.as_ref().and_then(|meta| meta.version.as_deref()) {
        out.push_str(" v");
        out.push_str(version);
    }
    if let Some(meta) = &meta {
        out.push_str("\n\n");
        out.push_str(match meta.kind {
            PackageKind::Registered => "Registered package",
            PackageKind::Dev => "Development dependency",
            PackageKind::Stdlib => "Standard library",
        });
    }
    if let Some(root) = root {
        // Normalized for the same reason the jump target is: a `dev`'d root
        // keeps the manifest's `../` spelling, which is not a path to show.
        out.push_str(&format!("\n\n`{}`", normalize_path(&root).display()));
    }
    Some(out)
}

/// Document links in a project file: every dependency name whose package the
/// server located becomes a link to that package's entry file.
///
/// The same target [`project_definition`] jumps to, through the same
/// [`package_entry`] join — the two must not drift, since a client offers them
/// on the same ctrl-click. Purely lexical over the buffer and the library map,
/// with no I/O, so a link is resolved eagerly and there is no
/// `documentLink/resolve`.
pub(crate) fn project_document_links<P: PackageSource>(
    text: &TextBuffer,
    encoding: PositionEncoding,
    packages: &P,
) -> Vec<DocumentLink> {
    let line_index = text.line_index();
    dep_entries(text)
        .into_iter()
        .filter_map(|dep| {
            let (entry, _) = package_entry(packages, &dep.name)?;
            Some(DocumentLink {
                range: to_range(dep.name_range, &line_index, encoding),
                target: Some(uri::from_path(&entry)?),
                tooltip: None,
                data: None,
            })
        })
        .collect()
}

/// [`project_document_links`] against a database snapshot.
pub(crate) fn project_document_links_via_db(
    snapshot: &Analysis,
    text: &TextBuffer,
    encoding: PositionEncoding,
) -> Vec<DocumentLink> {
    salsa::Cancelled::catch(AssertUnwindSafe(|| {
        project_document_links(text, encoding, snapshot)
    }))
    .unwrap_or_default()
}

fn to_range(range: TextRange, line_index: &LineIndex, encoding: PositionEncoding) -> Range {
    Range {
        start: line_index.byte_to_position(range.start().into(), encoding),
        end: line_index.byte_to_position(range.end().into(), encoding),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::index::{PackageIndex, harvest_package_named};
    use crate::resolve::ModulePath;

    use super::super::uri::to_path;
    use super::*;

    /// A unique temp directory removed on drop (mirrors `definition.rs`,
    /// avoiding a `tempfile` dev-dependency).
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("fatou-projnav-{}-{}", std::process::id(), n));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[derive(Default)]
    struct TestLib {
        packages: BTreeMap<String, Arc<PackageIndex>>,
        roots: BTreeMap<String, PathBuf>,
        deps: BTreeMap<String, PackageMeta>,
    }

    impl PackageSource for TestLib {
        fn package(&self, name: &str) -> Option<Arc<PackageIndex>> {
            self.packages.get(name).cloned()
        }
        fn package_root(&self, name: &str) -> Option<PathBuf> {
            self.roots.get(name).cloned()
        }
        fn workspace_member(&self, _path: &Path) -> Option<(Arc<PackageIndex>, ModulePath)> {
            None
        }
    }

    impl ProjectLibrary for TestLib {
        fn package_meta(&self, name: &str) -> Option<PackageMeta> {
            self.deps.get(name).cloned()
        }
    }

    fn meta(version: Option<&str>, kind: PackageKind) -> PackageMeta {
        PackageMeta {
            version: version.map(str::to_string),
            kind,
        }
    }

    /// A real on-disk `Greetings` package, harvested so its `DefLocation` is
    /// genuine rather than hand-built.
    fn greetings() -> (TempDir, TestLib, PathBuf) {
        let tmp = TempDir::new();
        let entry = tmp.path.join("src").join("Greetings.jl");
        fs::create_dir_all(entry.parent().unwrap()).unwrap();
        fs::write(&entry, "module Greetings\ngreet(name) = name\nend\n").unwrap();

        let mut lib = TestLib::default();
        lib.packages.insert(
            "Greetings".to_string(),
            Arc::new(harvest_package_named(&tmp.path, "Greetings")),
        );
        lib.roots.insert("Greetings".to_string(), tmp.path.clone());
        (tmp, lib, entry)
    }

    const PROJECT: &str = "\
name = \"Demo\"

[deps]
Greetings = \"1520ce14-60c1-5f80-bbc7-55ef81b5835c\"
";

    /// Go-to-definition at the position marked by `|` (the marker is stripped
    /// before parsing).
    fn def_at(marked: &str, lib: &impl PackageSource) -> Vec<Location> {
        let offset = marked.find('|').expect("a cursor marker");
        let text = TextBuffer::new(marked.replacen('|', "", 1));
        let position = text
            .line_index()
            .byte_to_position(offset, PositionEncoding::Utf16);
        project_definition(&text, position, PositionEncoding::Utf16, lib)
    }

    #[test]
    fn a_dependency_name_jumps_to_its_entry_file() {
        let (_tmp, lib, entry) = greetings();
        let marked = PROJECT.replace("Greetings =", "Greet|ings =");

        let locations = def_at(&marked, &lib);
        let [location] = &locations[..] else {
            panic!("expected exactly one location, got {locations:?}");
        };
        assert_eq!(to_path(&location.uri), Some(entry));
        // The `Greetings` of `module Greetings`, line 0, columns 7..16.
        assert_eq!(location.range.start, Position::new(0, 7));
        assert_eq!(location.range.end, Position::new(0, 16));
    }

    /// Every other position in the file is not a dependency name, and none of
    /// them may jump anywhere.
    #[test]
    fn nothing_else_in_the_file_jumps() {
        let (_tmp, lib, _entry) = greetings();
        for marked in [
            PROJECT.replace("name =", "na|me ="),
            PROJECT.replace("[deps]", "[de|ps]"),
            PROJECT.replace("\"1520ce14", "\"1520|ce14"),
        ] {
            assert!(def_at(&marked, &lib).is_empty(), "for {marked:?}");
        }
    }

    /// A package the server never harvested — a standard library with no
    /// located install, a missing depot slug — has no source to jump to.
    #[test]
    fn an_unharvested_dependency_has_no_definition() {
        let marked = PROJECT.replace("Greetings =", "Greet|ings =");
        assert!(def_at(&marked, &TestLib::default()).is_empty());
    }

    /// A `Project.toml` mid-edit does not parse, and a jump from one would be a
    /// guess. Its `toml-syntax` diagnostic is what reports the state it is in.
    #[test]
    fn a_broken_project_file_has_no_definition() {
        let (_tmp, lib, _entry) = greetings();
        let marked = "[deps\nGreet|ings = \"1520ce14\"\n";
        assert!(def_at(marked, &lib).is_empty());
    }

    // --- Hover --------------------------------------------------------------

    /// Hover at the position marked by `|`, as markdown.
    fn hover_at(marked: &str, lib: &TestLib) -> Option<String> {
        let offset = marked.find('|').expect("a cursor marker");
        let text = TextBuffer::new(marked.replacen('|', "", 1));
        let position = text
            .line_index()
            .byte_to_position(offset, PositionEncoding::Utf16);
        let hover = project_hover(&text, position, PositionEncoding::Utf16, lib)?;
        // The hover covers exactly the dependency name it reports on.
        assert_eq!(
            hover.range,
            Some(Range::new(Position::new(3, 0), Position::new(3, 9))),
        );
        match hover.contents {
            HoverContents::Markup(markup) => Some(markup.value),
            other => panic!("expected markdown, got {other:?}"),
        }
    }

    #[test]
    fn a_dependency_hover_reports_version_kind_and_path() {
        let (tmp, mut lib, _entry) = greetings();
        lib.deps.insert(
            "Greetings".to_string(),
            meta(Some("0.4.5"), PackageKind::Registered),
        );

        let marked = PROJECT.replace("Greetings =", "Greet|ings =");
        assert_eq!(
            hover_at(&marked, &lib),
            Some(format!(
                "**Greetings** v0.4.5\n\nRegistered package\n\n`{}`",
                tmp.path.display()
            )),
        );
    }

    /// Each line is dropped when unknown: an uninstantiated project pins no
    /// version, and a package whose source was not found has no path.
    #[test]
    fn an_unknown_version_or_path_is_simply_omitted() {
        let marked = PROJECT.replace("Greetings =", "Greet|ings =");

        let mut lib = TestLib::default();
        lib.deps
            .insert("Greetings".to_string(), meta(None, PackageKind::Stdlib));
        assert_eq!(
            hover_at(&marked, &lib),
            Some("**Greetings**\n\nStandard library".to_string()),
        );

        lib.deps
            .insert("Greetings".to_string(), meta(None, PackageKind::Dev));
        assert_eq!(
            hover_at(&marked, &lib),
            Some("**Greetings**\n\nDevelopment dependency".to_string()),
        );
    }

    /// A name the environment knows nothing about hovers to nothing. That it is
    /// absent from the manifest is `missing-from-manifest`'s report to make.
    #[test]
    fn a_dependency_nothing_is_known_about_has_no_hover() {
        let marked = PROJECT.replace("Greetings =", "Greet|ings =");
        assert_eq!(hover_at(&marked, &TestLib::default()), None);
    }

    // --- Document links -----------------------------------------------------

    /// One link per located dependency, covering exactly the name and pointing
    /// where go-to-definition jumps. `[weakdeps]` and `[extras]` name packages
    /// too, so they link as well.
    #[test]
    fn every_located_dependency_becomes_a_link() {
        let (_tmp, lib, entry) = greetings();
        let text = TextBuffer::new(format!(
            "{PROJECT}\n[extras]\nGreetings = \"1520ce14-60c1-5f80-bbc7-55ef81b5835c\"\n"
        ));

        let links = project_document_links(&text, PositionEncoding::Utf16, &lib);
        let target = uri::from_path(&entry).unwrap();
        assert_eq!(
            links
                .iter()
                .map(|link| (link.range, link.target.clone()))
                .collect::<Vec<_>>(),
            vec![
                (
                    Range::new(Position::new(3, 0), Position::new(3, 9)),
                    Some(target.clone())
                ),
                (
                    Range::new(Position::new(6, 0), Position::new(6, 9)),
                    Some(target)
                ),
            ],
        );
    }

    /// A dependency the server never located has nothing to link to, and a name
    /// under the cursor there jumps nowhere either — one join, one answer.
    #[test]
    fn an_unlocated_dependency_has_no_link() {
        let text = TextBuffer::new(PROJECT.to_string());
        assert!(
            project_document_links(&text, PositionEncoding::Utf16, &TestLib::default()).is_empty()
        );
    }

    #[test]
    fn a_broken_project_file_has_no_links() {
        let (_tmp, lib, _entry) = greetings();
        let text = TextBuffer::new("[deps\nGreetings = \"1520ce14\"\n".to_string());
        assert!(project_document_links(&text, PositionEncoding::Utf16, &lib).is_empty());
    }

    #[test]
    fn nothing_else_in_the_file_hovers() {
        let (_tmp, mut lib, _entry) = greetings();
        lib.deps.insert(
            "Greetings".to_string(),
            meta(Some("0.4.5"), PackageKind::Registered),
        );
        for marked in [
            PROJECT.replace("name =", "na|me ="),
            PROJECT.replace("[deps]", "[de|ps]"),
            PROJECT.replace("\"1520ce14", "\"1520|ce14"),
        ] {
            let offset = marked.find('|').expect("a cursor marker");
            let text = TextBuffer::new(marked.replacen('|', "", 1));
            let position = text
                .line_index()
                .byte_to_position(offset, PositionEncoding::Utf16);
            assert!(
                project_hover(&text, position, PositionEncoding::Utf16, &lib).is_none(),
                "for {marked:?}"
            );
        }
    }
}
