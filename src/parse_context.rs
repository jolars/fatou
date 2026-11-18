//! Static syntax-edition selection shared by disk adapters and live analysis.
//!
//! Text selection is pure. Filesystem discovery stays here in the root crate,
//! outside the parser and formatter engines. No route invokes Julia.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, hash_map::Entry};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::environment::{
    self, EnvContext, EnvironmentError, ProjectFile, Uuid, manifest_entry_tables,
    manifest_entry_uuid, parse_manifest_table, parse_project_text, syntax_version,
};
use crate::incremental::normalize_path;
use crate::julia_version::{Version, parse_compat};
use crate::parser::{ParseOptions, SyntaxEdition};
use crate::project::IncludeEdge;
use crate::syntax::SyntaxNode;

impl From<crate::cli::SyntaxEditionArg> for SyntaxEdition {
    fn from(value: crate::cli::SyntaxEditionArg) -> Self {
        match value {
            crate::cli::SyntaxEditionArg::Julia113 => Self::Julia113,
            crate::cli::SyntaxEditionArg::Julia114 => Self::Julia114,
            crate::cli::SyntaxEditionArg::Julia115 => Self::Julia115,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceRole {
    Package,
    Script,
}

impl SourceRole {
    pub const fn default_edition(self) -> SyntaxEdition {
        match self {
            Self::Package => SyntaxEdition::OLDEST,
            Self::Script => SyntaxEdition::LATEST,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditionIssue {
    pub path: PathBuf,
    pub range: Range<usize>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditionSelection {
    pub options: ParseOptions,
    pub certain: bool,
    pub issue: Option<EditionIssue>,
}

/// Semantic metadata identity for LSP result generations. Comments, patch
/// versions within an edition, and unrelated TOML fields leave it unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataSignature {
    Missing,
    Project {
        package: (ParseOptions, bool),
        script: (ParseOptions, bool),
        runtime: Option<crate::julia_version::VersionRange>,
        name: Option<String>,
        extensions: Vec<String>,
    },
    Manifest(BTreeMap<(String, String), (ParseOptions, bool)>),
    Invalid,
}

pub fn metadata_signature(path: &Path, text: Option<&str>) -> MetadataSignature {
    let Some(text) = text else {
        return MetadataSignature::Missing;
    };
    if environment::is_project_file(path) {
        let Ok(project) = parse_project_text(path, text) else {
            return MetadataSignature::Invalid;
        };
        let package = project_edition_from_metadata(path, &project, SourceRole::Package);
        let script = project_edition_from_metadata(path, &project, SourceRole::Script);
        return MetadataSignature::Project {
            package: (package.options, package.certain),
            script: (script.options, script.certain),
            runtime: project.julia_compat(),
            name: project.name.map(toml::Spanned::into_inner),
            extensions: project.extensions,
        };
    }
    let Ok(table) = parse_manifest_table(path, text) else {
        return MetadataSignature::Invalid;
    };
    let mut editions = BTreeMap::new();
    for (name, entry) in manifest_entry_tables(table.get_ref()) {
        if let Some(uuid) = manifest_entry_uuid(entry) {
            let selection = manifest_entry_edition(path, entry);
            editions
                .entry((name.to_owned(), uuid.to_string()))
                .or_insert((selection.options, selection.certain));
        }
    }
    MetadataSignature::Manifest(editions)
}

pub(crate) fn manifest_issues(path: &Path, table: &toml::de::DeTable<'_>) -> Vec<EditionIssue> {
    manifest_entry_tables(table)
        .filter(|(_, entry)| manifest_entry_uuid(entry).is_some())
        .filter_map(|(_, entry)| manifest_entry_edition(path, entry).issue)
        .collect()
}

/// Edition metadata problems for a syntactically valid TOML document.
pub fn metadata_issues(path: &Path, text: &str) -> Vec<EditionIssue> {
    if environment::is_project_file(path) {
        return project_edition(path, text, SourceRole::Package)
            .issue
            .into_iter()
            .collect();
    }
    parse_manifest_table(path, text)
        .map(|table| manifest_issues(path, table.get_ref()))
        .unwrap_or_default()
}

impl EditionSelection {
    pub fn known(edition: SyntaxEdition) -> Self {
        Self {
            options: ParseOptions {
                syntax_edition: edition,
            },
            certain: true,
            issue: None,
        }
    }

    fn invalid(path: &Path, edition: SyntaxEdition, range: Range<usize>, message: String) -> Self {
        Self {
            options: ParseOptions {
                syntax_edition: edition,
            },
            certain: false,
            issue: Some(EditionIssue {
                path: path.to_path_buf(),
                range,
                message,
            }),
        }
    }

    fn parse_error(path: &Path, edition: SyntaxEdition, error: EnvironmentError) -> Self {
        let range = match &error {
            EnvironmentError::Parse { span, .. } => span.clone().unwrap_or(0..0),
            _ => 0..0,
        };
        Self::invalid(path, edition, range, error.to_string())
    }

    pub fn require_certain(&self) -> Result<ParseOptions, String> {
        if self.certain {
            Ok(self.options)
        } else {
            Err(self.issue.as_ref().map_or_else(
                || "syntax edition is uncertain".to_string(),
                |issue| {
                    format!(
                        "cannot rewrite source while its syntax edition is uncertain: {}: {}",
                        issue.path.display(),
                        issue.message
                    )
                },
            ))
        }
    }
}

/// Select from one project's text. `[syntax]` is authoritative, even when its
/// value is invalid; a malformed explicit declaration never falls through to
/// `[compat]` or to an edition retained from an earlier editor state.
pub fn project_edition(path: &Path, text: &str, role: SourceRole) -> EditionSelection {
    match parse_project_text(path, text) {
        Ok(project) => project_edition_from_metadata(path, &project, role),
        Err(error) => EditionSelection::parse_error(path, role.default_edition(), error),
    }
}

pub(crate) fn project_edition_from_metadata(
    path: &Path,
    project: &ProjectFile,
    role: SourceRole,
) -> EditionSelection {
    let default = role.default_edition();
    if let Some(syntax) = &project.syntax {
        return match syntax.get_ref().as_deref() {
            Some(version) => version_selection(path, version, syntax.span(), default),
            None => EditionSelection::invalid(
                path,
                default,
                syntax.span(),
                "`[syntax].julia_version` must be a Julia version string".to_string(),
            ),
        };
    }
    if let Some(compat) = project.compat.get("julia") {
        return match parse_compat(compat.as_ref()) {
            Ok(range) => {
                EditionSelection::known(SyntaxEdition::normalized(range.min.major, range.min.minor))
            }
            Err(_) => EditionSelection::invalid(
                path,
                default,
                compat.span(),
                "invalid Julia compatibility range for syntax edition selection".to_string(),
            ),
        };
    }
    EditionSelection::known(default)
}

fn version_selection(
    path: &Path,
    text: &str,
    span: Range<usize>,
    default: SyntaxEdition,
) -> EditionSelection {
    match Version::parse_julia_number(text) {
        Some(version) => {
            EditionSelection::known(SyntaxEdition::normalized(version.major, version.minor))
        }
        None => EditionSelection::invalid(
            path,
            default,
            span,
            "invalid `[syntax].julia_version`; expected a Julia version".to_string(),
        ),
    }
}

/// Select the matching manifest entry by both name and UUID. The manifest's
/// top-level `julia_version` describes the runtime, never this edition.
pub fn manifest_edition(path: &Path, text: &str, name: &str, uuid: Uuid) -> EditionSelection {
    ManifestEditions::decode(path, text).select(path, name, uuid)
}

type DependencyEditions = BTreeMap<String, BTreeMap<Uuid, EditionSelection>>;

/// A decoded metadata file can select editions for every dependency it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManifestEditions(Result<DependencyEditions, EditionSelection>);

impl ManifestEditions {
    pub(crate) fn decode(path: &Path, text: &str) -> Self {
        let table = match parse_manifest_table(path, text) {
            Ok(table) => table,
            Err(error) => {
                return Self(Err(EditionSelection::parse_error(
                    path,
                    SyntaxEdition::OLDEST,
                    error,
                )));
            }
        };
        let mut editions = DependencyEditions::new();
        for (name, entry) in manifest_entry_tables(table.get_ref()) {
            if let Some(uuid) = manifest_entry_uuid(entry) {
                editions
                    .entry(name.to_owned())
                    .or_default()
                    .entry(uuid)
                    .or_insert_with(|| manifest_entry_edition(path, entry));
            }
        }
        Self(Ok(editions))
    }

    pub(crate) fn select(&self, path: &Path, name: &str, uuid: Uuid) -> EditionSelection {
        match &self.0 {
            Err(failure) => failure.clone(),
            Ok(editions) => editions
                .get(name)
                .and_then(|entries| entries.get(&uuid))
                .cloned()
                .unwrap_or_else(|| {
                    EditionSelection::invalid(
                        path,
                        SyntaxEdition::OLDEST,
                        0..0,
                        format!("manifest has no resolved entry for `{name}` ({uuid})"),
                    )
                }),
        }
    }
}

pub(crate) fn manifest_entry_edition(
    path: &Path,
    entry: &toml::de::DeTable<'_>,
) -> EditionSelection {
    let Some(version) = syntax_version(entry) else {
        return EditionSelection::known(SyntaxEdition::OLDEST);
    };
    if let Some(text) = *version.get_ref() {
        version_selection(path, text, version.span(), SyntaxEdition::OLDEST)
    } else {
        EditionSelection::invalid(
            path,
            SyntaxEdition::OLDEST,
            version.span(),
            "manifest `syntax.julia_version` must be a Julia version string".to_string(),
        )
    }
}

/// The metadata source of a loading context. Stored separately from source
/// text so buffers with the same edition can share parser and semantic work.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EditionSource {
    Project {
        path: PathBuf,
        role: SourceRole,
    },
    Manifest {
        path: PathBuf,
        name: String,
        uuid: Uuid,
    },
    Fixed(SyntaxEdition),
}

impl EditionSource {
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Project { path, .. } | Self::Manifest { path, .. } => Some(path),
            Self::Fixed(_) => None,
        }
    }

    pub fn select(&self, text: Option<&str>) -> EditionSelection {
        match self {
            Self::Fixed(edition) => EditionSelection::known(*edition),
            Self::Project { path, role } => text.map_or_else(
                || {
                    EditionSelection::invalid(
                        path,
                        role.default_edition(),
                        0..0,
                        "project file could not be read".to_string(),
                    )
                },
                |text| project_edition(path, text, *role),
            ),
            Self::Manifest { path, name, uuid } => text.map_or_else(
                || {
                    EditionSelection::invalid(
                        path,
                        SyntaxEdition::OLDEST,
                        0..0,
                        "manifest could not be read".to_string(),
                    )
                },
                |text| manifest_edition(path, text, name, *uuid),
            ),
        }
    }
}

pub(crate) fn dependency_edition_source(
    package: &environment::Package,
    root: &Path,
    manifest: Option<&Path>,
) -> EditionSource {
    if package.kind == environment::PackageKind::Stdlib {
        return environment::project_file_in(root).map_or(
            EditionSource::Fixed(SyntaxEdition::OLDEST),
            |path| EditionSource::Project {
                path,
                role: SourceRole::Package,
            },
        );
    }
    manifest.map_or(EditionSource::Fixed(package.syntax_edition), |path| {
        EditionSource::Manifest {
            path: path.to_path_buf(),
            name: package.name.clone(),
            uuid: package.uuid,
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceContext {
    pub entry: PathBuf,
    pub source: EditionSource,
}

impl SourceContext {
    pub(crate) fn owner_depth(&self, path: &Path) -> usize {
        match &self.source {
            EditionSource::Project {
                path: project,
                role: SourceRole::Package,
            } => project
                .parent()
                .filter(|root| path.starts_with(root))
                .map_or(0, |root| root.components().count()),
            _ => 0,
        }
    }

    pub fn selection(&self, buffers: &BTreeMap<PathBuf, Arc<str>>) -> EditionSelection {
        let text = self
            .source
            .path()
            .and_then(|path| metadata_text(path, buffers));
        self.source.select(text.as_deref())
    }
}

fn metadata_text(path: &Path, buffers: &BTreeMap<PathBuf, Arc<str>>) -> Option<Arc<str>> {
    buffers
        .get(path)
        .cloned()
        .or_else(|| std::fs::read_to_string(path).ok().map(Arc::from))
}

pub(crate) struct DiskEditionSelections<'a> {
    buffers: &'a BTreeMap<PathBuf, Arc<str>>,
    selections: HashMap<EditionSource, EditionSelection>,
    manifests: HashMap<PathBuf, Option<ManifestEditions>>,
}

impl<'a> DiskEditionSelections<'a> {
    pub(crate) fn new(buffers: &'a BTreeMap<PathBuf, Arc<str>>) -> Self {
        Self {
            buffers,
            selections: HashMap::new(),
            manifests: HashMap::new(),
        }
    }

    pub(crate) fn select(&mut self, source: &EditionSource) -> &EditionSelection {
        self.selections.entry(source.clone()).or_insert_with(|| {
            if let EditionSource::Manifest { path, name, uuid } = source {
                return self
                    .manifests
                    .entry(path.clone())
                    .or_insert_with(|| {
                        let text = metadata_text(path, self.buffers)?;
                        Some(ManifestEditions::decode(path, &text))
                    })
                    .as_ref()
                    .map_or_else(
                        || source.select(None),
                        |editions| editions.select(path, name, *uuid),
                    );
            }
            let text = source
                .path()
                .and_then(|path| metadata_text(path, self.buffers));
            source.select(text.as_deref())
        })
    }
}

/// Discover a direct file without harvesting packages or reading a manifest.
/// Known include contexts are supplied by the loaders and take precedence.
pub fn discover(
    path: &Path,
    ctx: &EnvContext,
    buffers: &BTreeMap<PathBuf, Arc<str>>,
) -> SourceContext {
    let selected = environment::explicitly_selected_project(ctx).map(|path| normalize_path(&path));
    discover_with_roots(
        path,
        ctx,
        selected.as_deref(),
        buffers,
        &mut BTreeMap::new(),
    )
    .0
}

pub(crate) fn nearest_project(
    path: &Path,
    buffers: &BTreeMap<PathBuf, Arc<str>>,
) -> Option<PathBuf> {
    let directory = path.parent().unwrap_or(path);
    directory.ancestors().find_map(|dir| {
        ["JuliaProject.toml", "Project.toml"]
            .into_iter()
            .map(|name| dir.join(name))
            .find(|path| buffers.contains_key(path) || path.is_file())
    })
}

/// A workspace package's main entry and declared extensions share an edition.
pub(crate) fn workspace_package_entries(
    project: &Path,
    buffers: &BTreeMap<PathBuf, Arc<str>>,
) -> Vec<SourceContext> {
    let Some(root) = project.parent() else {
        return Vec::new();
    };
    let Some(text) = metadata_text(project, buffers) else {
        return Vec::new();
    };
    // Ownership uses the name and entry point even if other fields are invalid.
    let Ok(table) = toml::de::DeTable::parse(&text) else {
        return Vec::new();
    };
    let table = table.get_ref();
    let Some(name) = table.get("name").and_then(|value| value.get_ref().as_str()) else {
        return Vec::new();
    };
    let main = root.join("src").join(format!("{name}.jl"));
    let is_file = |path: &Path| buffers.contains_key(path) || path.is_file();
    if !is_file(&main) {
        return Vec::new();
    }
    let source = EditionSource::Project {
        path: project.to_path_buf(),
        role: SourceRole::Package,
    };
    std::iter::once(main)
        .chain(
            environment::extension_names(table)
                .filter_map(|name| environment::extension_entry(root, name, is_file)),
        )
        .map(|entry| SourceContext {
            entry: normalize_path(&entry),
            source: source.clone(),
        })
        .collect()
}

fn script_source(
    path: &Path,
    ctx: &EnvContext,
    buffers: &BTreeMap<PathBuf, Arc<str>>,
) -> EditionSource {
    let nearest = nearest_project(path, buffers);
    let active = if ctx
        .julia_project
        .as_deref()
        .is_some_and(|value| !matches!(value.trim(), "" | "@."))
    {
        environment::discover_project(ctx).or(nearest)
    } else {
        nearest.or_else(|| environment::discover_project(ctx))
    };
    active.map_or(EditionSource::Fixed(SyntaxEdition::LATEST), |path| {
        EditionSource::Project {
            path: normalize_path(&path),
            role: SourceRole::Script,
        }
    })
}

fn discover_with_roots<'a>(
    path: &Path,
    ctx: &EnvContext,
    selected_project: Option<&Path>,
    buffers: &BTreeMap<PathBuf, Arc<str>>,
    entries: &'a mut BTreeMap<PathBuf, Vec<SourceContext>>,
) -> (SourceContext, &'a [SourceContext]) {
    let nearest = nearest_project(path, buffers).map(|nearest| {
        // A directory has one workspace owner. An explicit selection of its
        // alternate project file takes precedence over the default filename.
        selected_project
            .filter(|project| project.parent() == normalize_path(&nearest).parent())
            .map(Path::to_path_buf)
            .unwrap_or(nearest)
    });
    let package = nearest.as_ref().map_or(&[][..], |project| {
        entries
            .entry(project.clone())
            .or_insert_with(|| workspace_package_entries(project, buffers))
            .as_slice()
    });
    let is_package_source = nearest
        .as_deref()
        .and_then(Path::parent)
        .is_some_and(|root| {
            path.starts_with(root.join("src")) || path.starts_with(root.join("ext"))
        });
    let source = if let Some(main) = package.first()
        && is_package_source
    {
        main.source.clone()
    } else {
        script_source(path, ctx, buffers)
    };
    (
        SourceContext {
            entry: path.to_path_buf(),
            source,
        },
        package,
    )
}

fn disk_project_path(path: &Path) -> PathBuf {
    // On Unix, relative CLI targets resolve against getcwd's physical path.
    // Resolve an absolute project directory the same way, keeping the selected
    // filename. Live workspace paths retain the spelling supplied by the client.
    #[cfg(unix)]
    if path.is_absolute()
        && let (Some(directory), Some(name)) = (path.parent(), path.file_name())
        && let Ok(directory) = directory.canonicalize()
    {
        return directory.join(name);
    }
    normalize_path(path)
}

/// Discover manifest loading roots statically. Local path pins can include
/// files outside their package, so examine their closures. For installed
/// packages, only load roots that enclose a target.
pub(crate) fn manifest_contexts(
    paths: &[PathBuf],
    active_project: Option<&Path>,
    explicitly_selected: bool,
    mut ctx: EnvContext,
) -> (Vec<SourceContext>, BTreeMap<PathBuf, EditionIssue>) {
    use environment::PackageKind;

    if paths.is_empty() {
        return (Vec::new(), BTreeMap::new());
    }
    let depots = environment::depot_roots(&ctx);
    let mut projects = BTreeMap::<PathBuf, Vec<&PathBuf>>::new();
    if let Some(project) = active_project {
        projects.insert(project.to_path_buf(), paths.iter().collect());
    }
    if !explicitly_selected {
        for path in paths {
            ctx.workspace_root.clone_from(path);
            if let Some(project) = environment::discover_project(&ctx) {
                let project = normalize_path(&project);
                if Some(project.as_path()) != active_project {
                    projects.entry(project).or_default().push(path);
                }
            }
        }
    }
    let mut contexts = Vec::new();
    let mut issues = BTreeMap::new();
    for (project, targets) in projects {
        let Some(directory) = project.parent() else {
            continue;
        };
        let (manifest, mut packages) = match environment::load_manifest(directory, &depots) {
            Ok((Some(manifest), packages)) => (manifest, packages),
            Ok((None, _)) => continue,
            Err(error) => {
                let path = match &error {
                    EnvironmentError::Parse { path, .. } | EnvironmentError::Read { path, .. } => {
                        path.clone()
                    }
                };
                if let Some(issue) =
                    EditionSelection::parse_error(&path, SyntaxEdition::OLDEST, error).issue
                {
                    for target in targets {
                        issues
                            .entry(target.clone())
                            .or_insert_with(|| issue.clone());
                    }
                }
                continue;
            }
        };
        if packages
            .iter()
            .any(|package| package.kind == PackageKind::Stdlib)
            && let Some(install) = environment::locate_install(&ctx, &depots)
        {
            environment::resolve_stdlib_sources(&mut packages, &install);
        }
        for package in &packages {
            let Some(root) = package.source.as_deref().map(normalize_path) else {
                continue;
            };
            if package.kind != PackageKind::Dev && !paths.iter().any(|path| path.starts_with(&root))
            {
                continue;
            }
            let source = dependency_edition_source(package, &root, Some(&manifest));
            let entry = root.join("src").join(format!("{}.jl", package.name));
            contexts.extend(
                std::iter::once(entry)
                    .filter(|entry| entry.is_file())
                    .chain(package.extension_entries(&root))
                    .map(|entry| SourceContext {
                        entry,
                        source: source.clone(),
                    }),
            );
        }
    }
    (contexts, issues)
}

/// A disk adapter's immutable loading contexts. Source text is shared across
/// roots, while include discovery is performed under each root's edition.
#[derive(Debug, Clone, Default)]
pub struct ContextCatalog {
    pub files: BTreeMap<PathBuf, Vec<(SourceContext, EditionSelection)>>,
    pub sources: crate::project::scripts::ScriptSources,
    pub(crate) edges: BTreeMap<ParseOptions, BTreeMap<PathBuf, Arc<[IncludeEdge]>>>,
}

pub(crate) fn normalized_include_edges(root: &SyntaxNode, path: &Path) -> Arc<[IncludeEdge]> {
    let mut edges = crate::project::include_edges(root, path.parent());
    for edge in &mut edges {
        edge.target = edge.target.as_deref().map(normalize_path);
    }
    edges.into()
}

impl ContextCatalog {
    pub fn load(
        roots: &[SourceContext],
        buffers: &BTreeMap<PathBuf, Arc<str>>,
        override_edition: Option<SyntaxEdition>,
    ) -> Self {
        let mut out = Self::default();
        let mut editions = DiskEditionSelections::new(buffers);
        let overridden = override_edition.map(EditionSelection::known);
        let mut seen = HashSet::new();
        for root in roots {
            let mut context = root.clone();
            context.entry = normalize_path(&context.entry);
            if seen.insert(context.clone()) {
                let selection = overridden
                    .as_ref()
                    .unwrap_or_else(|| editions.select(&context.source));
                out.load_root(&context, selection, buffers);
            }
        }
        out.order_contexts();
        out
    }

    fn load_root(
        &mut self,
        context: &SourceContext,
        selection: &EditionSelection,
        buffers: &BTreeMap<PathBuf, Arc<str>>,
    ) {
        let mut pending = vec![context.entry.clone()];
        let mut visited = HashSet::new();
        while let Some(path) = pending.pop() {
            if !visited.insert(path.clone()) {
                continue;
            }
            if let Some(edges) = self.include_edges(&path, selection.options, buffers) {
                pending.extend(edges.iter().rev().filter_map(|edge| edge.target.clone()));
            }
            self.files
                .entry(path)
                .or_default()
                .push((context.clone(), selection.clone()));
        }
    }

    fn include_edges(
        &mut self,
        path: &Path,
        options: ParseOptions,
        buffers: &BTreeMap<PathBuf, Arc<str>>,
    ) -> Option<&[IncludeEdge]> {
        let text = self.sources.entry(path.to_path_buf()).or_insert_with(|| {
            buffers.get(path).cloned().map(Ok).unwrap_or_else(|| {
                if !path.is_file() {
                    return Err("not a regular file".to_string());
                }
                std::fs::read_to_string(path)
                    .map(Arc::from)
                    .map_err(|error| error.to_string())
            })
        });
        let text = text.as_ref().ok()?;
        Some(
            self.edges
                .entry(options)
                .or_default()
                .entry(path.to_path_buf())
                .or_insert_with(|| {
                    if !crate::project::may_contain_static_include(text) {
                        return Arc::from([]);
                    }
                    let parsed = crate::parser::parse_with_options(text, options);
                    normalized_include_edges(&parsed.cst, path)
                }),
        )
    }

    fn order_contexts(&mut self) {
        for (path, contexts) in &mut self.files {
            contexts.sort_by_key(|(context, _)| std::cmp::Reverse(context.owner_depth(path)));
        }
    }

    /// Discover configured programs and package roots once for a batch. Files
    /// outside those include closures receive their direct-file context.
    pub fn discover(
        paths: &[PathBuf],
        entries: &[PathBuf],
        buffers: &BTreeMap<PathBuf, Arc<str>>,
        override_edition: Option<SyntaxEdition>,
    ) -> Self {
        let paths: Vec<_> = paths.iter().map(|path| normalize_path(path)).collect();
        let mut roots = Vec::new();
        let mut package_entries = BTreeMap::new();
        for entry in entries {
            let entry = normalize_path(entry);
            roots.push(SourceContext {
                source: script_source(&entry, &EnvContext::from_process(entry.clone()), buffers),
                entry,
            });
        }
        let cwd = EnvContext::from_process(std::env::current_dir().unwrap_or_default());
        let selected = (!paths.is_empty())
            .then(|| environment::explicitly_selected_project(&cwd))
            .flatten()
            .map(|path| disk_project_path(&path));
        let active = selected.clone().or_else(|| {
            (!paths.is_empty())
                .then(|| environment::discover_project(&cwd))
                .flatten()
                .map(|path| normalize_path(&path))
        });
        let (manifest_roots, manifest_issues) =
            manifest_contexts(&paths, active.as_deref(), selected.is_some(), cwd);
        let manifest_entries: BTreeSet<_> = manifest_roots
            .iter()
            .map(|root| root.entry.clone())
            .collect();
        roots.extend(manifest_roots);

        let mut workspace_roots = HashSet::new();
        let mut add_workspace_roots = |package: &[SourceContext]| {
            roots.extend(
                package
                    .iter()
                    .filter(|root| {
                        !manifest_entries.contains(&root.entry)
                            && workspace_roots.insert((*root).clone())
                    })
                    .cloned(),
            );
        };
        if let Some(project) = active {
            add_workspace_roots(
                package_entries
                    .entry(project.clone())
                    .or_insert_with(|| workspace_package_entries(&project, buffers)),
            );
        }
        let mut direct = Vec::new();
        for path in paths {
            let (context, package) = discover_with_roots(
                &path,
                &EnvContext::from_process(path.clone()),
                selected.as_deref(),
                buffers,
                &mut package_entries,
            );
            add_workspace_roots(package);
            direct.push(context);
        }
        let mut out = Self::load(&roots, buffers, override_edition);
        direct.retain(|context| !out.files.contains_key(&context.entry));
        if !direct.is_empty() {
            out.load_direct_roots(direct, buffers, override_edition);
            out.order_contexts();
        }
        if override_edition.is_none() {
            for (path, issue) in manifest_issues {
                for (_, selection) in out.files.get_mut(&path).into_iter().flatten() {
                    selection.certain = false;
                    selection.issue.get_or_insert_with(|| issue.clone());
                }
            }
        }
        out
    }

    fn load_direct_roots(
        &mut self,
        direct: Vec<SourceContext>,
        buffers: &BTreeMap<PathBuf, Arc<str>>,
        override_edition: Option<SyntaxEdition>,
    ) {
        let mut editions = DiskEditionSelections::new(buffers);
        let overridden = override_edition.map(EditionSelection::known);
        let mut order = HashMap::new();
        let mut pending: Vec<_> = direct
            .into_iter()
            .enumerate()
            .filter_map(|(index, context)| {
                let Entry::Vacant(entry) = order.entry(context.entry.clone()) else {
                    return None;
                };
                entry.insert(index);
                Some(context)
            })
            .map(|context| {
                let selection = overridden
                    .as_ref()
                    .unwrap_or_else(|| editions.select(&context.source))
                    .clone();
                (context, selection)
            })
            .collect();
        while !pending.is_empty() {
            let included = self.included_direct_entries(&pending, buffers);
            let recursive_roots = if included.len() == pending.len() {
                self.recursive_direct_roots(&pending, buffers)
            } else {
                HashSet::new()
            };
            for (root, selection) in &pending {
                // A recursive include group may have no unique entry point.
                if recursive_roots.contains(&root.entry) || !included.contains(&root.entry) {
                    self.load_root(root, selection, buffers);
                }
            }
            // Reconsider files reached only through a proposed root that was
            // itself loaded as an include in a different edition.
            pending.retain(|(context, _)| !self.files.contains_key(&context.entry));
        }
        self.sources.retain(|path, _| self.files.contains_key(path));
        for edges in self.edges.values_mut() {
            edges.retain(|path, _| self.files.contains_key(path));
        }
        // Preserve the caller's primary context when inferred roots overlap,
        // even when an edition difference delays one root to a later round.
        for contexts in self.files.values_mut() {
            contexts.sort_by_key(|(context, _)| order.get(&context.entry));
        }
    }

    fn recursive_direct_roots(
        &mut self,
        pending: &[(SourceContext, EditionSelection)],
        buffers: &BTreeMap<PathBuf, Arc<str>>,
    ) -> HashSet<PathBuf> {
        let targets: HashSet<_> = pending.iter().map(|(root, _)| &root.entry).collect();
        let mut reachable = HashMap::new();
        for (root, selection) in pending {
            let mut visited = HashSet::new();
            let mut paths = vec![root.entry.clone()];
            while let Some(path) = paths.pop() {
                if visited.insert(path.clone())
                    && let Some(edges) = self.include_edges(&path, selection.options, buffers)
                {
                    paths.extend(edges.iter().filter_map(|edge| edge.target.clone()));
                }
            }
            visited.retain(|path| targets.contains(path));
            reachable.insert(&root.entry, visited);
        }
        // Follow root-to-root edges transitively. Each prospective root can
        // use a different edition, so its own include closure is only one
        // step in this graph.
        let recursive_groups: HashMap<_, _> = targets
            .iter()
            .map(|&entry| {
                let mut visited = HashSet::new();
                let mut work = vec![entry];
                while let Some(root) = work.pop() {
                    if visited.insert(root) {
                        work.extend(reachable[root].iter());
                    }
                }
                (entry, visited)
            })
            .collect();
        // Keep every member of a source recursive group. A target reached
        // from outside its group is reconsidered after those roots load.
        pending
            .iter()
            .filter(|(root, _)| {
                reachable.iter().all(|(origin, reached)| {
                    !reached.contains(&root.entry)
                        || recursive_groups[&root.entry].contains(*origin)
                })
            })
            .map(|(root, _)| root.entry.clone())
            .collect()
    }

    fn included_direct_entries(
        &mut self,
        pending: &[(SourceContext, EditionSelection)],
        buffers: &BTreeMap<PathBuf, Arc<str>>,
    ) -> HashSet<PathBuf> {
        if pending.len() <= 1 {
            return HashSet::new();
        }
        let targets: HashSet<_> = pending
            .iter()
            .map(|(root, _)| root.entry.as_path())
            .collect();
        let mut included = HashSet::new();
        let mut reached = HashMap::<ParseOptions, HashMap<PathBuf, Option<usize>>>::new();
        for (origin, (root, selection)) in pending.iter().enumerate() {
            let visited = reached.entry(selection.options).or_default();
            let mut paths = vec![root.entry.clone()];
            while let Some(path) = paths.pop() {
                match visited.entry(path.clone()) {
                    Entry::Vacant(entry) => {
                        entry.insert(Some(origin));
                    }
                    Entry::Occupied(mut entry)
                        if entry.get().is_some_and(|first| first != origin) =>
                    {
                        // A second origin suffices to distinguish a self-include
                        // from reachability from another target. Visit each
                        // file at most twice per edition.
                        entry.insert(None);
                    }
                    Entry::Occupied(_) => continue,
                }
                if path != root.entry && targets.contains(path.as_path()) {
                    included.insert(path.clone());
                }
                if let Some(edges) = self.include_edges(&path, selection.options, buffers) {
                    paths.extend(edges.iter().filter_map(|edge| edge.target.clone()));
                }
            }
        }
        included
    }

    pub fn selections(&self, path: &Path) -> &[(SourceContext, EditionSelection)] {
        self.files
            .get(&crate::incremental::normalize_path(path))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn options(&self, path: &Path) -> Vec<ParseOptions> {
        let mut seen = std::collections::BTreeSet::new();
        self.selections(path)
            .iter()
            .filter_map(|(_, selection)| {
                seen.insert(selection.options).then_some(selection.options)
            })
            .collect()
    }
}

/// Format every applicable view, requiring identical edits. An explicit CLI
/// override produces one certain view even if project metadata is malformed.
pub fn format_source(
    text: &str,
    style: crate::formatter::FormatStyle,
    safe: bool,
    selections: &[(SourceContext, EditionSelection)],
) -> Result<String, String> {
    let mut result = None;
    let mut editions = std::collections::BTreeSet::new();
    for (_, selection) in selections {
        let parse_options = selection.require_certain()?;
        if !editions.insert(parse_options) {
            continue;
        }
        let options = crate::formatter::FormatOptions {
            style,
            parse_options,
        };
        let formatted = if safe || selections.len() > 1 || parse_options != ParseOptions::default()
        {
            crate::formatter::format_verified_with_options(text, options)
                .map_err(|error| error.to_string())?
        } else {
            crate::formatter::format_with_options(text, options)
                .map_err(|error| error.to_string())?
        };
        if let Some(previous) = &result
            && previous != &formatted
        {
            return Err("source has conflicting formatting under its syntax editions".to_string());
        }
        result = Some(formatted);
    }
    result.ok_or_else(|| "source has no loading context".to_string())
}
