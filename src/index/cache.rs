//! An on-disk cache of harvested [`PackageIndex`] values.
//!
//! Harvesting a package re-parses its whole source with fatou's own parser;
//! Base and the stdlib are large, and the cost is paid again on every restart.
//! Because a [`PackageIndex`] is depot-independent (paths are package-relative,
//! see [`model`](super::model)), a harvested index can be serialized once and
//! reloaded across sessions as long as the package's *content* is unchanged.
//!
//! Each package is one file, `<root>/<name>/<key>.postcard`, so parallel harvest
//! tasks write without contending on a shared file. The `<key>` is a content
//! identity: a registered package's `git-tree-sha1` (immutable for a pinned
//! version) or, for Base/Core/stdlib, the Julia install version. The whole tree
//! is namespaced by [`CACHE_FORMAT`] and the crate version, so a fatou build
//! whose harvester or model changed never reads an index an older build wrote.
//!
//! Everything is best-effort: an unresolvable cache directory, an unwritable
//! file, or a corrupt entry disables that step and falls back to a live harvest,
//! never a panic.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::environment::{JuliaInstall, Package, PackageKind};

use super::model::PackageIndex;

/// The on-disk layout and serialized-shape version. Bump whenever a change makes
/// entries an older build wrote unreadable or wrong (a new `postcard` layout, a
/// [`PackageIndex`] field change). Recorded in every entry header and baked into
/// the cache path, so mismatched entries are ignored rather than mis-decoded.
const CACHE_FORMAT: u32 = 5;

/// The build identity stamped into every entry: `fatou@<version>`. Two builds at
/// the same [`CACHE_FORMAT`] can still harvest different shapes (the harvester
/// evolves between releases), so the version guards against trusting an entry a
/// different build wrote.
fn tool_fingerprint() -> String {
    format!("fatou@{}", env!("CARGO_PKG_VERSION"))
}

/// The path segment that isolates one build's entries from another's: a fresh
/// fatou version (or format bump) starts a clean subtree rather than colliding
/// with — or silently mis-reading — the previous one's files.
fn version_segment() -> String {
    format!("v{CACHE_FORMAT}-{}", env!("CARGO_PKG_VERSION"))
}

/// Serialized to disk (borrowed form, so a store never clones the index).
/// postcard is not self-describing: this must stay field-order- and
/// type-compatible with [`CacheEntryOwned`].
#[derive(Serialize)]
struct CacheEntryRef<'a> {
    format: u32,
    tool: &'a str,
    index: &'a PackageIndex,
}

/// Deserialized from disk. Mirror of [`CacheEntryRef`]; see its note.
#[derive(Deserialize)]
struct CacheEntryOwned {
    format: u32,
    tool: String,
    index: PackageIndex,
}

/// Uniquifies concurrent temp files without a clock or RNG (neither of which the
/// harvest path should need): a bare monotonic counter, paired with the PID.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The content key under which a package's harvested index is cached. A stable
/// identity that changes exactly when the package's source does, so a hit is
/// always for the right content and a bump (new pinned version, new Julia) is a
/// natural miss.
pub struct CacheKey;

impl CacheKey {
    /// The key for a manifest package, or `None` when it must not be cached: a
    /// `dev`'d package is edited live (re-harvested on save), and a registered
    /// package with no `git-tree-sha1` has no stable content identity. Stdlib
    /// packages are keyed through [`for_system`](Self::for_system) instead.
    pub fn for_package(package: &Package) -> Option<String> {
        match package.kind {
            PackageKind::Registered => package.tree_sha1.clone(),
            PackageKind::Dev | PackageKind::Stdlib => None,
        }
    }

    /// The key for any Base/Core/stdlib package of a located installation: its
    /// version. Every system package of one install shares this key (each under
    /// its own `<name>/` directory), and a Julia upgrade changes it, retiring the
    /// old install's entries wholesale.
    pub fn for_system(install: &JuliaInstall) -> String {
        format!("stdlib-{}", install.version)
    }
}

/// A handle to the on-disk index cache rooted at a version-namespaced directory.
/// Cheap to clone the path; holds no open files. Directories are created lazily
/// on the first [`store`](Self::store).
#[derive(Debug, Clone)]
pub struct IndexCache {
    /// `<base>/index/v<FORMAT>-<version>`; entries live at `<root>/<name>/<key>`.
    root: PathBuf,
}

impl IndexCache {
    /// Open the cache at the OS-resolved location, or `None` when none can be
    /// determined (caching is then simply off — callers treat `None` as disabled).
    pub fn open() -> Option<Self> {
        Some(Self::rooted(resolve_cache_dir()?))
    }

    /// Open the cache under an explicit base directory (`<base>/index/...`). For
    /// the `fatou index` CLI and tests; production uses [`open`](Self::open).
    pub fn open_at(base: impl Into<PathBuf>) -> Self {
        Self::rooted(base.into().join("index"))
    }

    /// Namespace an `.../index` directory by the current format and version.
    fn rooted(index_dir: PathBuf) -> Self {
        Self {
            root: index_dir.join(version_segment()),
        }
    }

    /// The file backing `(name, key)`.
    fn entry_path(&self, name: &str, key: &str) -> PathBuf {
        self.root.join(name).join(format!("{key}.postcard"))
    }

    /// Load the cached index for `(name, key)`, or `None` on any miss: absent
    /// file, decode failure, corruption, or a header from a different
    /// format/build. Never fails — a miss just means "harvest it".
    pub fn load(&self, name: &str, key: &str) -> Option<PackageIndex> {
        let path = self.entry_path(name, key);
        let bytes = std::fs::read(&path).ok()?;
        let entry: CacheEntryOwned = match postcard::from_bytes(&bytes) {
            Ok(entry) => entry,
            Err(err) => {
                log::debug!("index cache: undecodable entry {}: {err}", path.display());
                return None;
            }
        };
        if entry.format != CACHE_FORMAT || entry.tool != tool_fingerprint() {
            return None;
        }
        Some(entry.index)
    }

    /// Store `index` under `(name, key)`, best-effort. Writes to a unique temp
    /// file and renames it into place, so a concurrent load sees either the old
    /// entry or the whole new one, never a torn write. Any I/O error is logged
    /// and swallowed.
    pub fn store(&self, name: &str, key: &str, index: &PackageIndex) {
        let entry = CacheEntryRef {
            format: CACHE_FORMAT,
            tool: &tool_fingerprint(),
            index,
        };
        let bytes = match postcard::to_stdvec(&entry) {
            Ok(bytes) => bytes,
            Err(err) => {
                log::debug!("index cache: failed to encode {name}/{key}: {err}");
                return;
            }
        };
        let path = self.entry_path(name, key);
        let Some(parent) = path.parent() else {
            return;
        };
        if let Err(err) = std::fs::create_dir_all(parent) {
            log::debug!("index cache: cannot create {}: {err}", parent.display());
            return;
        }
        let tmp = parent.join(format!(
            ".{key}.{}.{}.tmp",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        if let Err(err) = std::fs::write(&tmp, &bytes) {
            log::debug!("index cache: cannot write {}: {err}", tmp.display());
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        if let Err(err) = std::fs::rename(&tmp, &path) {
            log::debug!("index cache: cannot rename into {}: {err}", path.display());
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// The OS cache location for the index, or `None` if none can be determined.
/// `FATOU_CACHE_HOME` overrides (its `index/` subdirectory is used); otherwise
/// the platform cache dir under `fatou/index` (e.g. `~/.cache/fatou/index`).
pub fn resolve_cache_dir() -> Option<PathBuf> {
    cache_dir_from(
        std::env::var_os("FATOU_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty()),
        dirs::cache_dir(),
    )
}

/// The pure core of [`resolve_cache_dir`], with the environment injected so the
/// precedence is testable without mutating process state.
fn cache_dir_from(env_override: Option<PathBuf>, os_cache: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(base) = env_override {
        return Some(base.join("index"));
    }
    os_cache.map(|dir| dir.join("fatou").join("index"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::Uuid;
    use crate::index::model::{
        DefLocation, Docstring, ExportedName, ModuleIndex, Span, Visibility,
    };
    use std::sync::Arc;

    fn nil_uuid() -> Uuid {
        "00000000-0000-0000-0000-000000000000".parse().unwrap()
    }

    /// A minimal but non-trivial index to round-trip.
    fn sample_index(name: &str) -> PackageIndex {
        let loc = DefLocation {
            file: PathBuf::from(format!("src/{name}.jl")),
            range: Span { start: 0, end: 3 },
        };
        PackageIndex {
            name: name.to_string(),
            root: ModuleIndex {
                name: name.to_string(),
                bare: false,
                loc: loc.clone(),
                doc: Some(Docstring {
                    text: "package docs".to_string(),
                    loc: loc.clone(),
                }),
                exports: vec![ExportedName {
                    name: "foo".to_string(),
                    visibility: Visibility::Exported,
                    loc,
                }],
                functions: Vec::new(),
                types: Vec::new(),
                consts: Vec::new(),
                macros: Vec::new(),
                submodules: Vec::new(),
                usings: Vec::new(),
                imported_names: Vec::new(),
            },
            members: vec![PathBuf::from(format!("src/{name}.jl"))],
            member_modules: Default::default(),
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn store_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::open_at(dir.path());
        let index = sample_index("Foo");
        cache.store("Foo", "abc123", &index);
        assert_eq!(cache.load("Foo", "abc123"), Some(index));
    }

    #[test]
    fn missing_entry_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::open_at(dir.path());
        assert_eq!(cache.load("Foo", "abc123"), None);
    }

    #[test]
    fn wrong_key_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::open_at(dir.path());
        cache.store("Foo", "abc123", &sample_index("Foo"));
        assert_eq!(cache.load("Foo", "def456"), None);
        assert_eq!(cache.load("Bar", "abc123"), None);
    }

    #[test]
    fn format_mismatch_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::open_at(dir.path());
        // Write an entry with a bogus format directly to the entry path.
        let bytes = postcard::to_stdvec(&CacheEntryRef {
            format: CACHE_FORMAT + 1,
            tool: &tool_fingerprint(),
            index: &sample_index("Foo"),
        })
        .unwrap();
        let path = cache.entry_path("Foo", "abc123");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(cache.load("Foo", "abc123"), None);
    }

    #[test]
    fn tool_mismatch_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::open_at(dir.path());
        let bytes = postcard::to_stdvec(&CacheEntryRef {
            format: CACHE_FORMAT,
            tool: "fatou@0.0.0-other",
            index: &sample_index("Foo"),
        })
        .unwrap();
        let path = cache.entry_path("Foo", "abc123");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(cache.load("Foo", "abc123"), None);
    }

    #[test]
    fn corrupt_entry_is_a_miss_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::open_at(dir.path());
        let path = cache.entry_path("Foo", "abc123");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not postcard at all").unwrap();
        assert_eq!(cache.load("Foo", "abc123"), None);
    }

    #[test]
    fn concurrent_stores_leave_a_readable_entry() {
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::open_at(dir.path());
        let index = sample_index("Foo");
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let cache = &cache;
                let index = &index;
                scope.spawn(move || cache.store("Foo", "abc123", index));
            }
        });
        assert_eq!(cache.load("Foo", "abc123"), Some(index));
    }

    #[test]
    fn cache_key_for_registered_uses_tree_sha1() {
        let package = Package {
            name: "Foo".to_string(),
            uuid: nil_uuid(),
            version: Some("1.2.3".to_string()),
            tree_sha1: Some("deadbeef".to_string()),
            deps: Vec::new(),
            kind: PackageKind::Registered,
            source: None,
        };
        assert_eq!(
            CacheKey::for_package(&package),
            Some("deadbeef".to_string())
        );
    }

    #[test]
    fn cache_key_for_dev_and_stdlib_is_none() {
        let mut package = Package {
            name: "Foo".to_string(),
            uuid: nil_uuid(),
            version: None,
            tree_sha1: None,
            deps: Vec::new(),
            kind: PackageKind::Dev,
            source: None,
        };
        assert_eq!(CacheKey::for_package(&package), None);
        package.kind = PackageKind::Stdlib;
        assert_eq!(CacheKey::for_package(&package), None);
    }

    #[test]
    fn env_override_takes_precedence_over_os_cache() {
        let resolved = cache_dir_from(
            Some(PathBuf::from("/custom/cache")),
            Some(PathBuf::from("/os/cache")),
        );
        assert_eq!(resolved, Some(PathBuf::from("/custom/cache/index")));
    }

    #[test]
    fn os_cache_is_used_without_override() {
        let resolved = cache_dir_from(None, Some(PathBuf::from("/os/cache")));
        assert_eq!(resolved, Some(PathBuf::from("/os/cache/fatou/index")));
    }

    #[test]
    fn no_location_disables_the_cache() {
        assert_eq!(cache_dir_from(None, None), None);
    }

    #[test]
    fn arc_wrapped_index_shares_the_stored_value() {
        // The harvest path stores through an `Arc<PackageIndex>`; loading yields
        // an owned value equal to the original.
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::open_at(dir.path());
        let index = Arc::new(sample_index("Foo"));
        cache.store("Foo", "k", &index);
        assert_eq!(cache.load("Foo", "k").map(Arc::new), Some(index));
    }
}
