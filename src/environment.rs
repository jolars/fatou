//! Julia environment resolution: locate the active project, read the pinned
//! package set from its manifest, and resolve each package to its on-disk
//! source directory in a depot.
//!
//! Fatou has no Julia runtime, so this mirrors what Julia's loader does using
//! only the filesystem. Discovery follows Julia's precedence: `JULIA_PROJECT`
//! first, then a walk-up from the workspace root, then the newest default
//! environment under `~/.julia/environments/`. Package sources live at
//! `<depot>/packages/<Name>/<slug>/`, where `<slug>` is derived from the
//! package UUID and its `git-tree-sha1` (see [`version_slug`]); we compute the
//! slug rather than scan because a package may have several versions installed.
//!
//! This module is intentionally standalone: it is not yet wired into the salsa
//! layer, the LSP, or the CLI. Later Phase 3/5 work consumes [`Environment`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A parsed 16-byte package UUID, stored in textual (big-endian) byte order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Uuid([u8; 16]);

impl Uuid {
    /// The 16 bytes in textual (big-endian) order.
    pub fn bytes(&self) -> [u8; 16] {
        self.0
    }
}

impl std::str::FromStr for Uuid {
    type Err = ();

    /// Parse the canonical `8-4-4-4-12` hyphenated form (hyphens optional).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut bytes = [0u8; 16];
        let mut nibbles = s.bytes().filter(|b| *b != b'-');
        for byte in bytes.iter_mut() {
            let hi = nibbles.next().and_then(hex_val).ok_or(())?;
            let lo = nibbles.next().and_then(hex_val).ok_or(())?;
            *byte = (hi << 4) | lo;
        }
        if nibbles.next().is_some() {
            return Err(()); // too many hex digits
        }
        Ok(Uuid(bytes))
    }
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// How a package's source was (or was not) located.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageKind {
    /// A registered package installed in a depot (`git-tree-sha1` present).
    Registered,
    /// A `dev`'d package referenced by `path`.
    Dev,
    /// A standard-library package (no `git-tree-sha1`, no `path`). Its source
    /// lives in the Julia installation, resolved by the later Base/stdlib work.
    Stdlib,
}

/// A single pinned package from the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub uuid: Uuid,
    pub version: Option<String>,
    pub tree_sha1: Option<String>,
    pub deps: Vec<String>,
    pub kind: PackageKind,
    /// The resolved package root (the directory that contains `src/`), if
    /// determinable. `None` for stdlib packages and for registered packages not
    /// found in any depot.
    pub source: Option<PathBuf>,
}

/// Which discovery strategy located the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvSource {
    JuliaProject,
    WorkspaceWalkUp,
    DefaultEnv,
}

/// A resolved Julia environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Environment {
    pub project_file: PathBuf,
    pub project_dir: PathBuf,
    pub manifest_file: Option<PathBuf>,
    pub name: Option<String>,
    pub uuid: Option<Uuid>,
    pub direct_deps: BTreeMap<String, Uuid>,
    pub packages: Vec<Package>,
    pub depots: Vec<PathBuf>,
    pub source: EnvSource,
}

/// Everything environment-dependent, injected so resolution stays testable
/// (no direct `std::env`/`$HOME` reads in the logic).
#[derive(Debug, Clone)]
pub struct EnvContext {
    pub workspace_root: PathBuf,
    pub julia_project: Option<String>,
    pub julia_depot_path: Option<String>,
    pub home: Option<PathBuf>,
}

impl EnvContext {
    /// Build a context from the process environment for the given workspace.
    pub fn from_process(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            julia_project: std::env::var("JULIA_PROJECT").ok(),
            julia_depot_path: std::env::var("JULIA_DEPOT_PATH").ok(),
            home: std::env::var_os("HOME").map(PathBuf::from),
        }
    }
}

#[derive(Debug)]
pub enum EnvironmentError {
    Read { path: PathBuf, message: String },
    Parse { path: PathBuf, message: String },
}

impl std::fmt::Display for EnvironmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnvironmentError::Read { path, message } => {
                write!(f, "failed to read {}: {message}", path.display())
            }
            EnvironmentError::Parse { path, message } => {
                write!(f, "failed to parse {}: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for EnvironmentError {}

const PROJECT_NAMES: [&str; 2] = ["JuliaProject.toml", "Project.toml"];
const MANIFEST_NAMES: [&str; 2] = ["JuliaManifest.toml", "Manifest.toml"];

/// Resolve the active Julia environment for `ctx`. Returns `Ok(None)` when no
/// project can be located by any strategy.
pub fn resolve(ctx: &EnvContext) -> Result<Option<Environment>, EnvironmentError> {
    let depots = depot_roots(ctx);
    let Some((project_file, source)) = locate_project(ctx, &depots) else {
        return Ok(None);
    };
    let project_dir = project_file
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();

    let (name, uuid, direct_deps) = parse_project(&project_file)?;
    let manifest_file = find_manifest(&project_dir);
    let packages = match &manifest_file {
        Some(path) => parse_manifest(path, &project_dir, &depots)?,
        None => Vec::new(),
    };

    Ok(Some(Environment {
        project_file,
        project_dir,
        manifest_file,
        name,
        uuid,
        direct_deps,
        packages,
        depots,
        source,
    }))
}

// --- Discovery -------------------------------------------------------------

/// Find the project file, following Julia's precedence.
fn locate_project(ctx: &EnvContext, depots: &[PathBuf]) -> Option<(PathBuf, EnvSource)> {
    if let Some(raw) = ctx.julia_project.as_deref() {
        let trimmed = raw.trim();
        if !trimmed.is_empty()
            && let Some(path) = from_julia_project(trimmed, ctx, depots)
        {
            return Some((path, EnvSource::JuliaProject));
        }
    }

    if let Some(path) = walk_up_for_project(&ctx.workspace_root) {
        return Some((path, EnvSource::WorkspaceWalkUp));
    }

    newest_default_env(ctx).map(|path| (path, EnvSource::DefaultEnv))
}

/// Interpret a `JULIA_PROJECT` value: `@.` (walk up), `@name` (shared env), or
/// a directory/file path.
fn from_julia_project(value: &str, ctx: &EnvContext, depots: &[PathBuf]) -> Option<PathBuf> {
    if value == "@." {
        return walk_up_for_project(&ctx.workspace_root);
    }
    if let Some(name) = value.strip_prefix('@') {
        return depots
            .iter()
            .find_map(|depot| project_file_in(&depot.join("environments").join(name)));
    }
    let path = PathBuf::from(value);
    if path.is_file() {
        return Some(path);
    }
    project_file_in(&path)
}

/// Walk up from `anchor` looking for a project file, à la `config::discover`.
fn walk_up_for_project(anchor: &Path) -> Option<PathBuf> {
    anchor.ancestors().find_map(project_file_in)
}

/// The project file within `dir`, honoring `JuliaProject.toml` precedence.
fn project_file_in(dir: &Path) -> Option<PathBuf> {
    PROJECT_NAMES
        .iter()
        .map(|name| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// The sibling manifest for a project directory, honoring name precedence and
/// falling back to the highest version-specific `Manifest-vX.Y.toml`.
fn find_manifest(project_dir: &Path) -> Option<PathBuf> {
    if let Some(path) = MANIFEST_NAMES
        .iter()
        .map(|name| project_dir.join(name))
        .find(|candidate| candidate.is_file())
    {
        return Some(path);
    }
    // Version-specific manifests (Julia 1.10.8+): pick the highest version.
    std::fs::read_dir(project_dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            let version = name
                .strip_prefix("Manifest-v")?
                .strip_suffix(".toml")
                .and_then(parse_version)?;
            Some((version, entry.path()))
        })
        .max_by_key(|(version, _)| *version)
        .map(|(_, path)| path)
}

/// The newest `~/.julia/environments/vX.Y` project, by `(major, minor)`.
fn newest_default_env(ctx: &EnvContext) -> Option<PathBuf> {
    let envs = ctx.home.as_ref()?.join(".julia").join("environments");
    let dir = std::fs::read_dir(&envs)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let version = parse_version(name.to_str()?.strip_prefix('v')?)?;
            Some((version, entry.path()))
        })
        .max_by_key(|(version, _)| *version)
        .map(|(_, path)| path)?;
    project_file_in(&dir)
}

/// Parse a `major.minor` (or longer) version prefix into a comparable tuple.
fn parse_version(s: &str) -> Option<(u32, u32)> {
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

// --- Depots ----------------------------------------------------------------

/// The ordered depot roots: `JULIA_DEPOT_PATH` (empty entries expand to the
/// default), falling back to `~/.julia`.
fn depot_roots(ctx: &EnvContext) -> Vec<PathBuf> {
    let default = ctx.home.as_ref().map(|home| home.join(".julia"));
    match ctx.julia_depot_path.as_deref() {
        Some(raw) if !raw.trim().is_empty() => raw
            .split(depot_separator())
            .flat_map(|entry| {
                if entry.is_empty() {
                    default.clone()
                } else {
                    Some(PathBuf::from(entry))
                }
            })
            .collect(),
        _ => default.into_iter().collect(),
    }
}

const fn depot_separator() -> char {
    if cfg!(windows) { ';' } else { ':' }
}

// --- Project.toml ----------------------------------------------------------

type ProjectMeta = (Option<String>, Option<Uuid>, BTreeMap<String, Uuid>);

fn parse_project(path: &Path) -> Result<ProjectMeta, EnvironmentError> {
    let table = read_toml(path)?;
    let name = table
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let uuid = table
        .get("uuid")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok());
    let mut direct_deps = BTreeMap::new();
    if let Some(deps) = table.get("deps").and_then(|v| v.as_table()) {
        for (dep_name, value) in deps {
            if let Some(uuid) = value.as_str().and_then(|s| s.parse().ok()) {
                direct_deps.insert(dep_name.clone(), uuid);
            }
        }
    }
    Ok((name, uuid, direct_deps))
}

// --- Manifest.toml ---------------------------------------------------------

fn parse_manifest(
    path: &Path,
    project_dir: &Path,
    depots: &[PathBuf],
) -> Result<Vec<Package>, EnvironmentError> {
    let table = read_toml(path)?;
    let mut packages = Vec::new();

    // Format 2.0 nests entries under a top-level `deps` table; format 1.0 puts
    // each package array at the top level.
    if let Some(deps) = table.get("deps").and_then(|v| v.as_table()) {
        for (name, value) in deps {
            collect_entries(name, value, project_dir, depots, &mut packages);
        }
    } else {
        for (name, value) in &table {
            collect_entries(name, value, project_dir, depots, &mut packages);
        }
    }

    packages.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(packages)
}

/// Push every package entry in `value` (an array of tables) into `packages`.
fn collect_entries(
    name: &str,
    value: &toml::Value,
    project_dir: &Path,
    depots: &[PathBuf],
    packages: &mut Vec<Package>,
) {
    let Some(entries) = value.as_array() else {
        return;
    };
    for entry in entries {
        if let Some(table) = entry.as_table()
            && let Some(package) = parse_entry(name, table, project_dir, depots)
        {
            packages.push(package);
        }
    }
}

fn parse_entry(
    name: &str,
    table: &toml::Table,
    project_dir: &Path,
    depots: &[PathBuf],
) -> Option<Package> {
    let uuid: Uuid = table.get("uuid").and_then(|v| v.as_str())?.parse().ok()?;
    let version = table
        .get("version")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let tree_sha1 = table
        .get("git-tree-sha1")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let path = table.get("path").and_then(|v| v.as_str());
    let deps = extract_deps(table.get("deps"));

    let kind = if path.is_some() {
        PackageKind::Dev
    } else if tree_sha1.is_some() {
        PackageKind::Registered
    } else {
        PackageKind::Stdlib
    };

    let source = match kind {
        PackageKind::Dev => Some(resolve_dev_path(project_dir, path?)),
        PackageKind::Registered => resolve_registered(name, uuid, tree_sha1.as_deref()?, depots),
        PackageKind::Stdlib => None,
    };

    Some(Package {
        name: name.to_string(),
        uuid,
        version,
        tree_sha1,
        deps,
        kind,
        source,
    })
}

/// A package's `deps` may be an array of names or a table (name -> uuid).
fn extract_deps(value: Option<&toml::Value>) -> Vec<String> {
    match value {
        Some(toml::Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        Some(toml::Value::Table(table)) => table.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// Resolve a `dev`'d package's root relative to the project directory.
fn resolve_dev_path(project_dir: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_dir.join(path)
    }
}

/// Locate a registered package's root by computing its version slug and probing
/// each depot in order.
fn resolve_registered(
    name: &str,
    uuid: Uuid,
    tree_sha1: &str,
    depots: &[PathBuf],
) -> Option<PathBuf> {
    let sha1 = parse_sha1(tree_sha1)?;
    let slug = version_slug(uuid, &sha1);
    depots
        .iter()
        .map(|depot| depot.join("packages").join(name).join(&slug))
        .find(|candidate| candidate.is_dir())
}

// --- Slug computation ------------------------------------------------------

const SLUG_CHARS: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// Julia's `version_slug(uuid, sha1)`: CRC-32C over the UUID's little-endian
/// bytes, continued over the tree hash, then base-62 encoded to 5 characters.
fn version_slug(uuid: Uuid, sha1: &[u8]) -> String {
    // Julia hashes the UUID's native (little-endian) in-memory representation,
    // which is the textual byte order reversed.
    let mut uuid_le = uuid.bytes();
    uuid_le.reverse();
    let crc = crc32c(&uuid_le, 0);
    let crc = crc32c(sha1, crc);
    slug(crc, 5)
}

fn slug(mut value: u32, len: usize) -> String {
    let base = SLUG_CHARS.len() as u32;
    let mut out = String::with_capacity(len);
    for _ in 0..len {
        let digit = (value % base) as usize;
        value /= base;
        out.push(SLUG_CHARS[digit] as char);
    }
    out
}

/// CRC-32C (Castagnoli), reflected, chainable via `crc`.
fn crc32c(bytes: &[u8], crc: u32) -> u32 {
    const POLY: u32 = 0x82F6_3B78;
    let mut c = !crc;
    for &byte in bytes {
        c ^= byte as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ POLY } else { c >> 1 };
        }
    }
    !c
}

/// Parse a 40-hex-character SHA1 into 20 bytes (textual order).
fn parse_sha1(s: &str) -> Option<[u8; 20]> {
    let mut bytes = [0u8; 20];
    let mut nibbles = s.bytes();
    for byte in bytes.iter_mut() {
        let hi = nibbles.next().and_then(hex_val)?;
        let lo = nibbles.next().and_then(hex_val)?;
        *byte = (hi << 4) | lo;
    }
    if nibbles.next().is_some() {
        return None;
    }
    Some(bytes)
}

// --- Shared helpers --------------------------------------------------------

fn read_toml(path: &Path) -> Result<toml::Table, EnvironmentError> {
    let text = std::fs::read_to_string(path).map_err(|err| EnvironmentError::Read {
        path: path.to_path_buf(),
        message: err.to_string(),
    })?;
    text.parse::<toml::Table>()
        .map_err(|err| EnvironmentError::Parse {
            path: path.to_path_buf(),
            message: err.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_uuid_roundtrip() {
        let uuid: Uuid = "1520ce14-60c1-5f80-bbc7-55ef81b5835c".parse().unwrap();
        assert_eq!(uuid.bytes()[0], 0x15);
        assert_eq!(uuid.bytes()[15], 0x5c);
    }

    #[test]
    fn rejects_malformed_uuid() {
        assert!("not-a-uuid".parse::<Uuid>().is_err());
        assert!("1520ce14".parse::<Uuid>().is_err());
    }

    #[test]
    fn crc32c_empty_is_zero() {
        assert_eq!(crc32c(b"", 0), 0);
    }

    #[test]
    fn crc32c_chains() {
        assert_eq!(
            crc32c(b"world", crc32c(b"hello ", 0)),
            crc32c(b"hello world", 0)
        );
    }

    /// Golden vector against a real depot entry:
    /// `AbstractTrees` -> on-disk slug `Ftf8W`.
    #[test]
    fn version_slug_golden() {
        let uuid: Uuid = "1520ce14-60c1-5f80-bbc7-55ef81b5835c".parse().unwrap();
        let sha1 = parse_sha1("2d9c9a55f9c93e8887ad391fbae72f8ef55e1177").unwrap();
        assert_eq!(version_slug(uuid, &sha1), "Ftf8W");
    }

    #[test]
    fn extract_deps_array_and_table() {
        let value: toml::Value = toml::from_str("deps = [\"A\", \"B\"]").unwrap();
        assert_eq!(extract_deps(value.get("deps")), vec!["A", "B"]);

        let value: toml::Value = toml::from_str("[deps]\nA = \"x\"\nB = \"y\"").unwrap();
        let mut got = extract_deps(value.get("deps"));
        got.sort();
        assert_eq!(got, vec!["A", "B"]);
    }

    #[test]
    fn classifies_manifest_entries() {
        let text = r#"
            julia_version = "1.11.7"
            manifest_format = "2.0"

            [[deps.AbstractTrees]]
            git-tree-sha1 = "2d9c9a55f9c93e8887ad391fbae72f8ef55e1177"
            uuid = "1520ce14-60c1-5f80-bbc7-55ef81b5835c"
            version = "0.4.5"

            [[deps.Dates]]
            uuid = "ade2ca70-3891-5945-98fb-dc099432e06a"

            [[deps.Local]]
            deps = ["Dates"]
            path = "vendor/Local"
            uuid = "00000000-0000-0000-0000-000000000001"
        "#;
        let table: toml::Table = text.parse().unwrap();
        let project_dir = Path::new("/proj");
        let deps = table.get("deps").and_then(|v| v.as_table()).unwrap();
        let mut packages = Vec::new();
        for (name, value) in deps {
            collect_entries(name, value, project_dir, &[], &mut packages);
        }
        packages.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(packages.len(), 3);
        let by_name = |n: &str| packages.iter().find(|p| p.name == n).unwrap();

        assert_eq!(by_name("AbstractTrees").kind, PackageKind::Registered);
        assert_eq!(by_name("Dates").kind, PackageKind::Stdlib);
        assert_eq!(by_name("Dates").source, None);

        let local = by_name("Local");
        assert_eq!(local.kind, PackageKind::Dev);
        assert_eq!(local.deps, vec!["Dates"]);
        assert_eq!(local.source, Some(PathBuf::from("/proj/vendor/Local")));
    }

    #[test]
    fn depot_roots_fall_back_to_home() {
        let ctx = EnvContext {
            workspace_root: PathBuf::from("/ws"),
            julia_project: None,
            julia_depot_path: None,
            home: Some(PathBuf::from("/home/u")),
        };
        assert_eq!(depot_roots(&ctx), vec![PathBuf::from("/home/u/.julia")]);
    }

    #[test]
    fn depot_roots_expand_empty_entry_to_default() {
        let sep = depot_separator();
        let ctx = EnvContext {
            workspace_root: PathBuf::from("/ws"),
            julia_project: None,
            julia_depot_path: Some(format!("/custom{sep}")),
            home: Some(PathBuf::from("/home/u")),
        };
        assert_eq!(
            depot_roots(&ctx),
            vec![PathBuf::from("/custom"), PathBuf::from("/home/u/.julia")]
        );
    }

    #[test]
    fn parse_version_orders_correctly() {
        assert!(parse_version("1.11") > parse_version("1.7"));
        assert!(parse_version("2.0") > parse_version("1.99"));
        assert_eq!(parse_version("nope"), None);
    }
}
