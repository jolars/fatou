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

use lsp_types::{Location, Position};

use crate::incremental::{Analysis, normalize_path};
use crate::index::Span;
use crate::project_files::dep_at;
use crate::resolve::PackageSource;
use crate::text::{PositionEncoding, TextBuffer};

use super::definition::site_locations;

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
    // Normalized, because a `dev`'d dependency's root is the manifest's `path`
    // joined to the project directory and keeps its `../` spelling. A URI
    // carrying one names the right file under a different string, which is
    // exactly the identity the client keys its open documents on.
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
}
