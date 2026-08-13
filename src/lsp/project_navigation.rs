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
//! A `Manifest.toml` answers exactly one of them, [`manifest_document_links`],
//! and it rides no database at all: a manifest is never written to one (nothing
//! reads a manifest without an environment resolve), and a link on a `path`
//! entry is decided by the buffer and the filesystem alone.

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};

use lsp_types::{
    DocumentLink, Hover, HoverContents, InlayHint, InlayHintLabel, InlayHintTooltip, Location,
    MarkupContent, MarkupKind, Position, Range,
};
use rowan::TextRange;

use crate::environment::{PackageKind, PackageMeta, project_file_in, resolve_dev_path};
use crate::incremental::{Analysis, normalize_path};
use crate::index::Span;
use crate::project_files::{dep_at, dep_entries, manifest_paths};
use crate::resolve::PackageSource;
use crate::text::{PositionEncoding, TextBuffer};

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
    let Some(dep) = dep_at(&text.text(), offset) else {
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
    let dep = dep_at(&text.text(), offset)?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: dep_markdown(&dep.name, library)?,
        }),
        range: Some(to_range(dep.name_range, line_index, encoding)),
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
    dep_entries(&text.text())
        .into_iter()
        .filter_map(|dep| {
            let (entry, _) = package_entry(packages, &dep.name)?;
            Some(DocumentLink {
                range: to_range(dep.name_range, line_index, encoding),
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

/// Document links in an open *manifest*: every `path = "..."` entry links to
/// the `dev`'d package it pins.
///
/// The only feature a manifest answers, and the only thing in one that anchors
/// anywhere: `path` is the sole entry field naming something outside the file.
/// It needs no library and no database — the path is resolved exactly as
/// [`resolve_dev_path`] resolves it during a harvest, so the link and the
/// environment cannot disagree about where the package is.
///
/// The target is the root's *project file*, not the root itself: a `dev`'d root
/// is a package, what identifies a package is its `Project.toml`, and a client
/// cannot open a directory. A root with neither spelling of one is linked
/// as-is rather than not at all — that it is not a package is a fact worth
/// walking into, and the `path` may simply be a typo.
pub(crate) fn manifest_document_links(
    text: &TextBuffer,
    path: &Path,
    encoding: PositionEncoding,
) -> Vec<DocumentLink> {
    // A manifest sits beside its project file, so its own directory is the
    // project directory `resolve_dev_path` resolves against.
    let Some(base_dir) = uri::anchor_dir(path) else {
        return Vec::new();
    };
    let line_index = text.line_index();
    manifest_paths(&text.text())
        .into_iter()
        .filter_map(|entry| {
            // An empty path resolves to the manifest's own directory, which is
            // no package and no link.
            if entry.path.is_empty() {
                return None;
            }
            // Normalized for the reason the jump targets are: a `path` entry
            // keeps its `../` spelling, which the filesystem resolves and a URI
            // does not, and a client comparing URIs textually would open a
            // second tab onto a file it already has.
            let root = normalize_path(&resolve_dev_path(base_dir, &entry.path));
            let target = project_file_in(&root).unwrap_or(root);
            Some(DocumentLink {
                range: to_range(entry.range, line_index, encoding),
                target: Some(uri::from_path(&target)?),
                tooltip: None,
                data: None,
            })
        })
        .collect()
}

/// Inlay hints in a project file: each dependency's resolved version, after its
/// UUID.
///
/// The `[deps]` table is almost entirely UUID, and the one fact a reader wants
/// from it — which version am I actually on — lives in the `Manifest.toml` next
/// door, which nobody opens. Hover answers that one dependency at a time and
/// only when asked; this answers all of them at once and passively, which is
/// what the two features are respectively for.
///
/// Only the version is shown. The kind belongs to the question "tell me about
/// *this* dependency", which is hover's, and repeating "Registered package"
/// down the table is noise. A dependency with no resolved version — an
/// uninstantiated project — gets no hint rather than an empty one.
pub(crate) fn project_inlay_hints<L: ProjectLibrary>(
    text: &TextBuffer,
    range: Range,
    encoding: PositionEncoding,
    library: &L,
) -> Vec<InlayHint> {
    let line_index = text.line_index();
    // The client asks for its viewport and re-asks on scroll and on edit, so a
    // hint outside it is dropped rather than computed and thrown away.
    let start = line_index.position_to_byte(range.start, encoding);
    let end = line_index.position_to_byte(range.end, encoding);
    dep_entries(&text.text())
        .into_iter()
        .filter_map(|dep| {
            let at = usize::from(dep.uuid_range.end());
            if at < start || at > end {
                return None;
            }
            let version = library.package_meta(&dep.name)?.version?;
            Some(InlayHint {
                position: line_index.byte_to_position(at, encoding),
                label: InlayHintLabel::String(format!("v{version}")),
                // No kind: neither `Type` nor `Parameter` describes a version,
                // and the spec has the client style an absent one sensibly.
                kind: None,
                text_edits: None,
                // The rest of what hover would say, so the hint itself hovers.
                tooltip: dep_markdown(&dep.name, library).map(|value| {
                    InlayHintTooltip::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value,
                    })
                }),
                padding_left: Some(true),
                padding_right: None,
                data: None,
            })
        })
        .collect()
}

/// [`project_inlay_hints`] against a database snapshot.
pub(crate) fn project_inlay_hints_via_db(
    snapshot: &Analysis,
    text: &TextBuffer,
    range: Range,
    encoding: PositionEncoding,
) -> Vec<InlayHint> {
    salsa::Cancelled::catch(AssertUnwindSafe(|| {
        project_inlay_hints(text, range, encoding, snapshot)
    }))
    .unwrap_or_default()
}

fn to_range(range: TextRange, line_index: &TextBuffer, encoding: PositionEncoding) -> Range {
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
        let text = TextBuffer::new(PROJECT);
        assert!(
            project_document_links(&text, PositionEncoding::Utf16, &TestLib::default()).is_empty()
        );
    }

    #[test]
    fn a_broken_project_file_has_no_links() {
        let (_tmp, lib, _entry) = greetings();
        let text = TextBuffer::new("[deps\nGreetings = \"1520ce14\"\n");
        assert!(project_document_links(&text, PositionEncoding::Utf16, &lib).is_empty());
    }

    // --- Manifest document links --------------------------------------------

    /// A manifest naming `dev`'d `path` entries: one package with a project
    /// file of its own, one directory without.
    fn manifest(dir: &Path) -> String {
        format!(
            "manifest_format = \"2.0\"\n\n\
             [[deps.Greetings]]\npath = \"Greetings\"\n\n\
             [[deps.Bare]]\npath = \"{}\"\n\n\
             [[deps.AbstractTrees]]\ngit-tree-sha1 = \"deadbeef\"\n",
            dir.join("Bare").display().to_string().replace('\\', "/"),
        )
    }

    fn manifest_links(text: &str, path: &Path) -> Vec<(Range, String)> {
        manifest_document_links(&TextBuffer::new(text), path, PositionEncoding::Utf16)
            .into_iter()
            .map(|link| (link.range, link.target.expect("a link target").to_string()))
            .collect()
    }

    /// The link covers the path text, and lands on the root's project file —
    /// the thing that makes the root a package, and a file a client can open.
    /// A root with no project file is linked as-is; a registered entry pins no
    /// path and links nowhere.
    #[test]
    fn every_manifest_path_links_to_the_package_it_pins() {
        let tmp = TempDir::new();
        fs::create_dir_all(tmp.path.join("Greetings")).unwrap();
        fs::write(
            tmp.path.join("Greetings/JuliaProject.toml"),
            "name = \"Greetings\"\n",
        )
        .unwrap();
        fs::create_dir_all(tmp.path.join("Bare")).unwrap();

        let text = manifest(&tmp.path);
        let links = manifest_links(&text, &tmp.path.join("Manifest.toml"));
        assert_eq!(
            links,
            vec![
                (
                    Range::new(Position::new(3, 8), Position::new(3, 17)),
                    uri::from_path(&tmp.path.join("Greetings/JuliaProject.toml"))
                        .unwrap()
                        .to_string(),
                ),
                (
                    Range::new(
                        Position::new(6, 8),
                        Position::new(
                            6,
                            u32::try_from(tmp.path.join("Bare").display().to_string().len() + 8)
                                .unwrap(),
                        ),
                    ),
                    uri::from_path(&tmp.path.join("Bare")).unwrap().to_string(),
                ),
            ],
        );
    }

    /// The path is resolved the way the environment resolves a `dev`'d root:
    /// against the manifest's own directory, `../` spellings collapsed rather
    /// than carried into the URI.
    #[test]
    fn a_relative_manifest_path_normalizes_against_the_manifest() {
        let tmp = TempDir::new();
        let nested = tmp.path.join("MyPkg");
        fs::create_dir_all(&nested).unwrap();
        let text = "[[deps.Greetings]]\npath = \"../Greetings\"\n";

        let links = manifest_links(text, &nested.join("Manifest.toml"));
        assert_eq!(
            links,
            vec![(
                Range::new(Position::new(1, 8), Position::new(1, 20)),
                uri::from_path(&tmp.path.join("Greetings"))
                    .unwrap()
                    .to_string(),
            )],
            "the link names `<tmp>/Greetings`, not `<tmp>/MyPkg/../Greetings`"
        );
    }

    /// A synthetic path stands in for a non-`file` URI and anchors no relative
    /// path, exactly as it does for a Julia document's includes. An empty path
    /// names the manifest's own directory, which is no package.
    #[test]
    fn a_manifest_with_nothing_to_anchor_to_has_no_links() {
        let untitled = <lsp_types::Uri as std::str::FromStr>::from_str("untitled:Untitled-1")
            .expect("a valid uri");
        let text = "[[deps.Greetings]]\npath = \"../Greetings\"\n";
        assert!(manifest_links(text, &uri::to_path_or_synthetic(&untitled)).is_empty());

        let tmp = TempDir::new();
        let empty = "[[deps.Greetings]]\npath = \"\"\n";
        assert!(manifest_links(empty, &tmp.path.join("Manifest.toml")).is_empty());
    }

    #[test]
    fn a_broken_manifest_has_no_links() {
        let tmp = TempDir::new();
        let text = "[[deps.Greetings\npath = \"../Greetings\"\n";
        assert!(manifest_links(text, &tmp.path.join("Manifest.toml")).is_empty());
    }

    // --- Inlay hints --------------------------------------------------------

    /// The whole viewport, for a test that is not about the range filter.
    const WHOLE_FILE: Range = Range {
        start: Position {
            line: 0,
            character: 0,
        },
        end: Position {
            line: u32::MAX,
            character: 0,
        },
    };

    /// A hint's label. `InlayHintLabel` implements no `PartialEq`, so a test
    /// compares the string it carries.
    fn label(hint: &InlayHint) -> &str {
        match &hint.label {
            InlayHintLabel::String(text) => text,
            other => panic!("expected a plain string label, got {other:?}"),
        }
    }

    /// A two-dependency project: `Greetings` resolved to a version, `Silent`
    /// pinned but never instantiated.
    fn two_deps() -> (TextBuffer, TestLib) {
        let text = TextBuffer::new(format!(
            "{PROJECT}Silent = \"682c06a0-de6a-54ab-a142-c8b1cf79cde6\"\n"
        ));
        let mut lib = TestLib::default();
        lib.deps.insert(
            "Greetings".to_string(),
            meta(Some("0.4.5"), PackageKind::Registered),
        );
        lib.deps
            .insert("Silent".to_string(), meta(None, PackageKind::Registered));
        (text, lib)
    }

    /// The version lands after the UUID, which is the end of the line — and a
    /// dependency with no resolved version contributes nothing rather than an
    /// empty hint.
    #[test]
    fn each_resolved_dependency_shows_its_version() {
        let (text, lib) = two_deps();
        let hints = project_inlay_hints(&text, WHOLE_FILE, PositionEncoding::Utf16, &lib);

        let [hint] = &hints[..] else {
            panic!("expected exactly one hint, got {hints:?}");
        };
        assert_eq!(label(hint), "v0.4.5");
        let text = text.text();
        let deps_line = text.lines().nth(3).expect("the Greetings line");
        assert_eq!(
            hint.position,
            Position::new(3, u32::try_from(deps_line.len()).unwrap()),
        );
        assert_eq!(hint.padding_left, Some(true));
        // The hint itself hovers, with the rest of what hover would say.
        assert!(matches!(
            &hint.tooltip,
            Some(InlayHintTooltip::MarkupContent(markup))
                if markup.value.contains("Registered package"),
        ));
    }

    /// The client asks for its viewport and re-asks on every scroll, so a hint
    /// outside it is never built.
    #[test]
    fn hints_outside_the_viewport_are_dropped() {
        let (text, mut lib) = two_deps();
        lib.deps.insert(
            "Silent".to_string(),
            meta(Some("0.21.4"), PackageKind::Registered),
        );

        // Only the second dependency's line.
        let viewport = Range::new(Position::new(4, 0), Position::new(5, 0));
        let hints = project_inlay_hints(&text, viewport, PositionEncoding::Utf16, &lib);
        assert_eq!(hints.iter().map(label).collect::<Vec<_>>(), vec!["v0.21.4"],);
    }

    #[test]
    fn a_broken_project_file_has_no_hints() {
        let (_tmp, mut lib, _entry) = greetings();
        lib.deps.insert(
            "Greetings".to_string(),
            meta(Some("0.4.5"), PackageKind::Registered),
        );
        let text = TextBuffer::new("[deps\nGreetings = \"1520ce14\"\n");
        assert!(project_inlay_hints(&text, WHOLE_FILE, PositionEncoding::Utf16, &lib).is_empty());
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
