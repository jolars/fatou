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
use std::io::Read;
use std::path::{Path, PathBuf};

use toml::Spanned;

use crate::julia_version::{VersionRange, parse_compat};

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

/// A located Julia installation whose plain Base/stdlib sources fatou can
/// harvest. Found from the filesystem alone, without running Julia.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JuliaInstall {
    /// The installation prefix (`<prefix>/bin/julia`, `<prefix>/share/julia`).
    pub prefix: PathBuf,
    /// `<prefix>/share/julia`.
    pub share: PathBuf,
    /// `<share>/base`, holding `Base.jl`, `boot.jl`, `exports.jl`, etc.
    pub base_dir: PathBuf,
    /// `<share>/stdlib/vX.Y`, holding one directory per standard-library package.
    pub stdlib_dir: PathBuf,
    /// The stdlib version, e.g. `1.11`, taken from the `stdlib/vX.Y` directory.
    pub version: String,
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
    /// The Julia installation whose Base/stdlib sources back this environment,
    /// if one could be located. `None` falls back to the baked-in export list.
    pub install: Option<JuliaInstall>,
}

/// The package under development: the workspace's own `Project.toml` package,
/// whose `src/` tree fatou indexes like a depot package so its top-level symbols
/// resolve across the package's files. `root` is the directory containing `src/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevPackage {
    pub name: String,
    pub root: PathBuf,
}

impl Environment {
    /// The package under development, if this environment *is* a package project
    /// (a named `Project.toml` with a matching `src/<Name>.jl` entry file). A
    /// bare shared environment (`DefaultEnv`) or a nameless project has none.
    ///
    /// The entry-file check is what distinguishes a package project from a plain
    /// environment that merely carries a `name`; only the former has a module
    /// tree to harvest.
    pub fn dev_package(&self) -> Option<DevPackage> {
        self.entry_file().filter(|entry| entry.is_file())?;
        Some(DevPackage {
            name: self.name.clone()?,
            root: self.project_dir.clone(),
        })
    }

    /// The `src/<Name>.jl` entry file this project's package *would* have,
    /// whether or not it exists. `None` when the project is not a package
    /// candidate at all: a shared default environment, or one with no `name`.
    ///
    /// Split out of [`dev_package`](Self::dev_package) so a consumer can tell
    /// "not a package" from "a package whose entry file is missing" — the
    /// latter is a defect worth reporting, the former is not.
    pub fn entry_file(&self) -> Option<PathBuf> {
        if self.source == EnvSource::DefaultEnv {
            return None;
        }
        let name = self.name.as_ref()?;
        Some(self.project_dir.join("src").join(format!("{name}.jl")))
    }
}

/// Everything environment-dependent, injected so resolution stays testable
/// (no direct `std::env`/`$HOME` reads in the logic).
#[derive(Debug, Clone)]
pub struct EnvContext {
    pub workspace_root: PathBuf,
    pub julia_project: Option<String>,
    pub julia_depot_path: Option<String>,
    pub home: Option<PathBuf>,
    /// `JULIA_BINDIR`: an explicit `<prefix>/bin` override for locating the
    /// installation, taking precedence over juliaup and `PATH`.
    pub julia_bindir: Option<String>,
    /// The process `PATH`, searched for the `julia` executable as a last resort.
    pub path: Option<String>,
}

impl EnvContext {
    /// Build a context from the process environment for the given workspace.
    pub fn from_process(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            julia_project: std::env::var("JULIA_PROJECT").ok(),
            julia_depot_path: std::env::var("JULIA_DEPOT_PATH").ok(),
            home: std::env::var_os("HOME").map(PathBuf::from),
            julia_bindir: std::env::var("JULIA_BINDIR").ok(),
            path: std::env::var("PATH").ok(),
        }
    }
}

#[derive(Debug)]
pub enum EnvironmentError {
    Read {
        path: PathBuf,
        message: String,
    },
    Parse {
        path: PathBuf,
        message: String,
        /// The byte range in the file's text that the failure points at, when
        /// the parser could locate one. `None` for a failure with no position
        /// (an unexpected end of input, typically), which a consumer reporting
        /// a range must fall back for.
        span: Option<std::ops::Range<usize>>,
    },
}

impl std::fmt::Display for EnvironmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnvironmentError::Read { path, message } => {
                write!(f, "failed to read {}: {message}", path.display())
            }
            EnvironmentError::Parse { path, message, .. } => {
                write!(f, "failed to parse {}: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for EnvironmentError {}

const PROJECT_NAMES: [&str; 2] = ["JuliaProject.toml", "Project.toml"];
const MANIFEST_NAMES: [&str; 2] = ["JuliaManifest.toml", "Manifest.toml"];

/// Whether `path` names a file that steers environment resolution: a project
/// file (`Project.toml`/`JuliaProject.toml`) or a manifest (`Manifest.toml`/
/// `JuliaManifest.toml`, or a version-specific `Manifest-vX.Y.toml`). The LSP
/// uses this to escalate a watched-file change to a full environment
/// re-resolve instead of a workspace re-harvest.
pub fn is_environment_file(path: &Path) -> bool {
    is_project_file(path) || is_manifest_file(path)
}

/// Whether `path` names a project file (`Project.toml`/`JuliaProject.toml`).
/// Distinguished from a manifest because the two carry different schemas: only
/// the project file is read against a typed one.
pub fn is_project_file(path: &Path) -> bool {
    file_name(path).is_some_and(|name| PROJECT_NAMES.contains(&name))
}

/// Whether `path` names a manifest (`Manifest.toml`/`JuliaManifest.toml`, or a
/// version-specific `Manifest-vX.Y.toml`).
pub fn is_manifest_file(path: &Path) -> bool {
    file_name(path).is_some_and(|name| {
        MANIFEST_NAMES.contains(&name)
            || name
                .strip_prefix("Manifest-v")
                .and_then(|rest| rest.strip_suffix(".toml"))
                .and_then(parse_version)
                .is_some()
    })
}

fn file_name(path: &Path) -> Option<&str> {
    path.file_name().and_then(|name| name.to_str())
}

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
    let mut packages = match &manifest_file {
        Some(path) => parse_manifest(path, &project_dir, &depots)?,
        None => Vec::new(),
    };

    let install = locate_install(ctx, &depots);
    if let Some(install) = &install {
        resolve_stdlib_sources(&mut packages, install);
    }

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
        install,
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

// --- Julia installation ----------------------------------------------------

/// Locate the active Julia installation without running Julia, trying (in
/// order): the `JULIA_BINDIR` override, the juliaup default channel, then the
/// `julia` executable on `PATH`. Returns `None` when none resolves to a tree
/// with a readable `base/Base.jl`.
pub fn locate_install(ctx: &EnvContext, depots: &[PathBuf]) -> Option<JuliaInstall> {
    install_from_bindir(ctx)
        .or_else(|| install_from_juliaup(depots))
        .or_else(|| install_from_path(ctx))
}

/// `<JULIA_BINDIR>/../share/julia`.
fn install_from_bindir(ctx: &EnvContext) -> Option<JuliaInstall> {
    let bindir = ctx.julia_bindir.as_deref().map(str::trim)?;
    if bindir.is_empty() {
        return None;
    }
    let prefix = Path::new(bindir).parent()?;
    install_from_share(prefix.join("share").join("julia"))
}

/// Read `<depot>/juliaup/juliaup.json`, follow the default channel's version to
/// its install directory, and take its bundled `share/julia`.
fn install_from_juliaup(depots: &[PathBuf]) -> Option<JuliaInstall> {
    for depot in depots {
        let juliaup = depot.join("juliaup");
        let Ok(text) = std::fs::read_to_string(juliaup.join("juliaup.json")) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(install) = juliaup_install_dir(&json, &juliaup) else {
            continue;
        };
        if let Some(found) = install_from_share(install.join("share").join("julia")) {
            return Some(found);
        }
    }
    None
}

/// The default channel's install directory from a parsed `juliaup.json`, joined
/// under `juliaup/` (where the `Path` values are relative).
fn juliaup_install_dir(json: &serde_json::Value, juliaup: &Path) -> Option<PathBuf> {
    let default = json.get("Default")?.as_str()?;
    let version = json
        .get("InstalledChannels")?
        .get(default)?
        .get("Version")?
        .as_str()?;
    let rel = json
        .get("InstalledVersions")?
        .get(version)?
        .get("Path")?
        .as_str()?;
    Some(juliaup.join(rel))
}

/// Find `julia` on `PATH`, resolve symlinks and shell wrappers, and take
/// `<prefix>/share/julia` from the `<prefix>/bin/julia` layout.
fn install_from_path(ctx: &EnvContext) -> Option<JuliaInstall> {
    let path = ctx.path.as_deref()?;
    let exe = if cfg!(windows) { "julia.exe" } else { "julia" };
    let julia = path
        .split(depot_separator())
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(dir).join(exe))
        .find(|candidate| candidate.is_file())?;
    let real = std::fs::canonicalize(&julia).ok()?;
    // On NixOS (and some distros) `julia` is a shell wrapper that `exec`s the
    // real binary in a different prefix; follow the chain to the ELF.
    let real = std::fs::canonicalize(follow_wrapper(&real, 8)).unwrap_or(real);
    let prefix = real.parent()?.parent()?; // <prefix>/bin/julia -> <prefix>
    install_from_share(prefix.join("share").join("julia"))
}

/// Follow a chain of shell wrappers (each ending in `exec "<path>"`) to the real
/// executable, bounded by `depth`. Non-scripts and unparseable wrappers stop.
fn follow_wrapper(path: &Path, depth: u8) -> PathBuf {
    if depth == 0 || !is_shebang(path) {
        return path.to_path_buf();
    }
    match std::fs::read_to_string(path)
        .ok()
        .as_deref()
        .and_then(exec_target)
    {
        Some(target) if target != path => follow_wrapper(&target, depth - 1),
        _ => path.to_path_buf(),
    }
}

/// Whether `path`'s first two bytes are `#!` (a shell wrapper, not an ELF).
fn is_shebang(path: &Path) -> bool {
    let mut buf = [0u8; 2];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut buf))
        .is_ok()
        && &buf == b"#!"
}

/// The target of the last `exec <path>` line in a wrapper script (quoted or a
/// bare first token).
fn exec_target(script: &str) -> Option<PathBuf> {
    let line = script
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with("exec "))?;
    let after = line.trim_start().strip_prefix("exec ")?.trim_start();
    let target = match after.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => after.split_whitespace().next()?,
    };
    (!target.is_empty()).then(|| PathBuf::from(target))
}

/// Build and validate a [`JuliaInstall`] from a candidate `share/julia`
/// directory: it must hold `base/Base.jl` and a `stdlib/vX.Y` directory.
fn install_from_share(share: PathBuf) -> Option<JuliaInstall> {
    let base_dir = share.join("base");
    if !base_dir.join("Base.jl").is_file() {
        return None;
    }
    let (version, stdlib_dir) = newest_stdlib(&share.join("stdlib"))?;
    let prefix = share.parent()?.parent()?.to_path_buf(); // <prefix>/share/julia
    Some(JuliaInstall {
        prefix,
        base_dir,
        stdlib_dir,
        version,
        share,
    })
}

/// The highest `vMAJOR.MINOR` directory under `stdlib/`, with its `X.Y` string.
fn newest_stdlib(stdlib: &Path) -> Option<(String, PathBuf)> {
    std::fs::read_dir(stdlib)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let raw = name.to_str()?.strip_prefix('v')?;
            let version = parse_version(raw)?;
            Some((version, raw.to_string(), entry.path()))
        })
        .max_by_key(|(version, _, _)| *version)
        .map(|(_, raw, path)| (raw, path))
}

/// Point each stdlib package at its source under the installation's `stdlib`.
fn resolve_stdlib_sources(packages: &mut [Package], install: &JuliaInstall) {
    for pkg in packages
        .iter_mut()
        .filter(|p| p.kind == PackageKind::Stdlib)
    {
        let dir = install.stdlib_dir.join(&pkg.name);
        if dir.is_dir() {
            pkg.source = Some(dir);
        }
    }
}

// --- Project.toml ----------------------------------------------------------

/// A `name = "value"` table whose keys *and* values carry their byte span: the
/// shape of `[deps]` and `[compat]` (and of `[extras]`/`[weakdeps]`, which the
/// project-file checks add to the schema when they need them).
pub(crate) type SpannedMap = BTreeMap<Spanned<String>, Spanned<String>>;

/// The `Project.toml` schema, with a span on everything a diagnostic anchors
/// on. Spans are byte offsets into the text the parse saw; the parse does not
/// retain that text, so a consumer reporting ranges holds its own copy (see
/// [`parse_project_text`]).
///
/// **Unknown keys are ignored, deliberately.** This is *Julia's* schema, not
/// fatou's: it already carries `authors`, `version`, `targets`, `workspace`,
/// `apps`, `extensions`, and it grows faster than fatou does.
/// `deny_unknown_fields` — the right policy for fatou's own `fatou.toml`
/// (`crate::config::RawConfig`) — would turn each future Julia key into a hard
/// resolve failure, and [`resolve`]'s callers swallow `Err`, so the failure
/// would surface as a silently missing index rather than as an error.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct ProjectFile {
    pub name: Option<Spanned<String>>,
    /// Kept textual rather than a parsed [`Uuid`]: a malformed UUID must stay a
    /// dropped value (this module's "a malformed on-disk file is input"
    /// invariant), not a parse failure, and a diagnostic about one wants the
    /// spelling as written.
    pub uuid: Option<Spanned<String>>,
    #[serde(default)]
    pub deps: SpannedMap,
    #[serde(default)]
    pub compat: SpannedMap,
}

impl ProjectFile {
    /// Drop the spans, yielding what [`resolve`] stores on [`Environment`]. A
    /// `uuid` that does not parse is dropped rather than raised.
    fn into_meta(self) -> ProjectMeta {
        let direct_deps = self
            .deps
            .into_iter()
            .filter_map(|(name, uuid)| Some((name.into_inner(), uuid.into_inner().parse().ok()?)))
            .collect();
        (
            self.name.map(Spanned::into_inner),
            self.uuid.and_then(|uuid| uuid.into_inner().parse().ok()),
            direct_deps,
        )
    }

    /// The `[compat].julia` range, if present and parseable.
    pub(crate) fn julia_compat(&self) -> Option<VersionRange> {
        parse_compat(self.compat.get("julia")?.as_ref()).ok()
    }
}

type ProjectMeta = (Option<String>, Option<Uuid>, BTreeMap<String, Uuid>);

fn parse_project(path: &Path) -> Result<ProjectMeta, EnvironmentError> {
    Ok(parse_project_text(path, &read_text(path)?)?.into_meta())
}

/// Parse a project file's text against [`ProjectFile`]. The one project-file
/// parse in the crate; `path` is carried only for the error.
pub(crate) fn parse_project_text(path: &Path, text: &str) -> Result<ProjectFile, EnvironmentError> {
    parse_toml(path, text)
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

// --- Julia version target --------------------------------------------------

/// Discover the project's declared Julia support range for version-compat
/// linting, without the full [`resolve`] harvest. Walks up from `anchor` for a
/// project file and reads its `[compat].julia`; when the project declares no
/// `julia` compat, falls back to the sibling manifest's resolved `julia_version`
/// (a point version, treated as an exact range). Returns `None` when neither is
/// present or parses — leaving the `julia-version-compat` rule silent.
pub fn discover_julia_target(anchor: &Path) -> Option<VersionRange> {
    let project = walk_up_for_project(anchor)?;
    if let Some(range) = read_text(&project)
        .and_then(|text| parse_project_text(&project, &text))
        .ok()
        .and_then(|project| project.julia_compat())
    {
        return Some(range);
    }
    let project_dir = project.parent()?;
    let manifest = find_manifest(project_dir)?;
    read_toml(&manifest)
        .ok()
        .and_then(manifest_range_from_table)
}

/// The manifest's top-level `julia_version` as an exact range, if present.
fn manifest_range_from_table(table: toml::Table) -> Option<VersionRange> {
    let version = table.get("julia_version")?.as_str()?.parse().ok()?;
    Some(VersionRange::exact(version))
}

// --- Shared helpers --------------------------------------------------------

/// Read a TOML file, keeping the source text: spans are byte offsets into it,
/// so a consumer that reports ranges needs the same string the parse saw.
pub(crate) fn read_text(path: &Path) -> Result<String, EnvironmentError> {
    std::fs::read_to_string(path).map_err(|err| EnvironmentError::Read {
        path: path.to_path_buf(),
        message: err.to_string(),
    })
}

/// Deserialize `text` into `T`, turning a TOML syntax or schema failure into a
/// span-carrying [`EnvironmentError::Parse`].
fn parse_toml<T: serde::de::DeserializeOwned>(
    path: &Path,
    text: &str,
) -> Result<T, EnvironmentError> {
    toml::from_str(text).map_err(|err| EnvironmentError::Parse {
        path: path.to_path_buf(),
        // `to_string` renders a multi-line snippet with a caret diagram, which
        // is right for a terminal and wrong for a one-line diagnostic; `span`
        // now carries the location the caret was drawing.
        message: err.message().to_string(),
        span: err.span(),
    })
}

/// Read and parse a file as an untyped table. The manifest's reader: nothing
/// anchors a diagnostic inside a manifest beyond its syntax, so it needs no
/// schema of its own — which also sidesteps its two incompatible layouts (see
/// [`parse_manifest`]).
fn read_toml(path: &Path) -> Result<toml::Table, EnvironmentError> {
    parse_toml(path, &read_text(path)?)
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
    fn reads_julia_compat_range_from_project() {
        let project = parse_project_text(
            Path::new("Project.toml"),
            "[compat]\njulia = \"1.6\"\nDataFrames = \"1.5\"",
        )
        .unwrap();
        let range = project.julia_compat().unwrap();
        assert_eq!(range.min, crate::julia_version::Version::new(1, 6, 0));
    }

    #[test]
    fn missing_julia_compat_is_none() {
        let project =
            parse_project_text(Path::new("Project.toml"), "[compat]\nDataFrames = \"1.5\"")
                .unwrap();
        assert!(project.julia_compat().is_none());
    }

    /// `Project.toml` is *Julia's* schema, not fatou's: it already carries keys
    /// fatou has no interest in, and it grows faster than fatou does. An
    /// unknown key must be ignored, never a parse failure — `resolve`'s callers
    /// swallow `Err`, so rejecting one would show up as a silently missing
    /// index rather than as an error.
    #[test]
    fn unknown_julia_keys_still_resolve() {
        let text = r#"
name = "Demo"
uuid = "11111111-2222-3333-4444-555555555555"
version = "0.1.0"
authors = ["Someone <someone@example.com>"]

[deps]
Dates = "ade2ca70-3891-5945-98fb-dc099432e06a"

[compat]
julia = "1.10"

[extras]
Test = "8dfed614-e22c-5e08-85e1-65c5234f0b40"

[weakdeps]
Plots = "91a5bcdd-55d7-5caf-9e0b-520d859cae80"

[extensions]
DemoPlotsExt = "Plots"

[targets]
test = ["Test"]

[sources]
Local = { path = "vendor/Local" }

[workspace]
projects = ["sub"]
"#;
        let project = parse_project_text(Path::new("Project.toml"), text).unwrap();
        assert_eq!(
            project.name.as_ref().map(Spanned::get_ref),
            Some(&"Demo".to_string())
        );
        assert!(project.deps.contains_key("Dates"));
        assert_eq!(
            project.julia_compat().map(|range| range.min),
            Some(crate::julia_version::Version::new(1, 10, 0)),
            "the keys fatou does know survive alongside the ones it does not"
        );
    }

    /// The span a semantic finding points at is a byte offset into the text the
    /// parse saw, for keys as well as values.
    #[test]
    fn spans_locate_keys_and_values() {
        let text = "name = \"Demo\"\n\n[deps]\nDates = \"ade2ca70-3891-5945-98fb-dc099432e06a\"\n";
        let project = parse_project_text(Path::new("Project.toml"), text).unwrap();

        let name = project.name.as_ref().unwrap();
        assert_eq!(&text[name.span()], "\"Demo\"");

        let (dep, uuid) = project.deps.iter().next().unwrap();
        assert_eq!(&text[dep.span()], "Dates");
        assert_eq!(
            &text[uuid.span()],
            "\"ade2ca70-3891-5945-98fb-dc099432e06a\""
        );
    }

    /// A syntax error carries the offset that a diagnostic anchors on.
    #[test]
    fn a_syntax_error_carries_its_span() {
        let text = "name = \"Demo\"\nuuid = \n";
        let err = parse_project_text(Path::new("Project.toml"), text).unwrap_err();
        let EnvironmentError::Parse { span, .. } = err else {
            panic!("expected a parse error, got {err:?}");
        };
        let span = span.expect("a span for a mid-file syntax error");
        assert!(span.start >= text.find("uuid").unwrap(), "{span:?}");
    }

    #[test]
    fn reads_manifest_julia_version_as_exact() {
        let table: toml::Table = "julia_version = \"1.11.7\"\nmanifest_format = \"2.0\""
            .parse()
            .unwrap();
        let range = manifest_range_from_table(table).unwrap();
        assert_eq!(
            range,
            VersionRange::exact(crate::julia_version::Version::new(1, 11, 7))
        );
    }

    #[test]
    fn discover_prefers_compat_over_manifest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Project.toml"),
            "name = \"Demo\"\n[compat]\njulia = \"1.10\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Manifest.toml"),
            "julia_version = \"1.11.7\"\nmanifest_format = \"2.0\"\n",
        )
        .unwrap();
        let range = discover_julia_target(dir.path()).unwrap();
        assert_eq!(range.min, crate::julia_version::Version::new(1, 10, 0));
    }

    #[test]
    fn discover_falls_back_to_manifest_without_compat() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Project.toml"), "name = \"Demo\"\n").unwrap();
        std::fs::write(
            dir.path().join("Manifest.toml"),
            "julia_version = \"1.9.2\"\nmanifest_format = \"2.0\"\n",
        )
        .unwrap();
        let range = discover_julia_target(dir.path()).unwrap();
        assert_eq!(
            range,
            VersionRange::exact(crate::julia_version::Version::new(1, 9, 2))
        );
    }

    #[test]
    fn discover_returns_none_without_project() {
        let dir = tempfile::tempdir().unwrap();
        assert!(discover_julia_target(dir.path()).is_none());
    }

    #[test]
    fn depot_roots_fall_back_to_home() {
        let ctx = EnvContext {
            workspace_root: PathBuf::from("/ws"),
            julia_project: None,
            julia_depot_path: None,
            home: Some(PathBuf::from("/home/u")),
            julia_bindir: None,
            path: None,
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
            julia_bindir: None,
            path: None,
        };
        assert_eq!(
            depot_roots(&ctx),
            vec![PathBuf::from("/custom"), PathBuf::from("/home/u/.julia")]
        );
    }

    #[test]
    fn classifies_environment_files() {
        for name in [
            "Project.toml",
            "JuliaProject.toml",
            "Manifest.toml",
            "JuliaManifest.toml",
            "Manifest-v1.11.toml",
        ] {
            assert!(
                is_environment_file(&PathBuf::from("/ws").join(name)),
                "{name} steers resolution"
            );
        }
        for name in ["a.jl", "Cargo.toml", "Manifest-vX.toml", "Manifest-v1.11"] {
            assert!(
                !is_environment_file(&PathBuf::from("/ws").join(name)),
                "{name} does not steer resolution"
            );
        }
    }

    #[test]
    fn parse_version_orders_correctly() {
        assert!(parse_version("1.11") > parse_version("1.7"));
        assert!(parse_version("2.0") > parse_version("1.99"));
        assert_eq!(parse_version("nope"), None);
    }
}
