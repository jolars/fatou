//! The linting pipeline: parse each file, run the enabled rules, and report a
//! status. Parse diagnostics block the rules for a file (they need a clean
//! tree) but are still reported, under the [`PARSE_ERROR_RULE`] pseudo-rule.

use std::path::{Path, PathBuf};

use rayon::prelude::*;

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};

use crate::config::LintConfig;
use crate::file_discovery::{ExcludeFilter, FileDiscoveryError, collect_julia_files};
use crate::index::{DeclaredDeps, HarvestedLibrary, PackageIndex, build_system_index};
use crate::julia_version::VersionRange;
use crate::parse_context::{
    ContextCatalog, DiskEditionSelections, EditionSelection, EditionSource, SourceContext,
};
use crate::parser::{ParseOptions, SyntaxEdition};
use crate::project::scripts::ScriptAnalysis;
use rowan::TextRange;

use crate::linter::diagnostic::{Diagnostic, Severity, ViolationData};
use crate::linter::include_graph::{
    IncludeProblem, include_problems_from_catalog, include_problems_with_options,
};
use crate::linter::rules::{ResolutionContext, ResolvedRules, RuleContext};
use crate::linter::suppression::SuppressionMap;
use crate::semantic::SemanticModel;
use crate::syntax::SyntaxNode;

/// The pseudo-rule id under which parse diagnostics are reported.
pub const PARSE_ERROR_RULE: &str = "parse-error";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LintStatus {
    /// No lint findings. Informational analysis notices may still be present.
    Clean,
    Findings {
        count: usize,
    },
    ParseDiagnostics {
        count: usize,
    },
}

#[derive(Debug, Clone)]
pub struct LintFileReport {
    pub path: Option<PathBuf>,
    /// Counts parse errors or lint findings; analysis notices do not affect it.
    pub status: LintStatus,
    /// Findings and informational analysis notices. Use [`Diagnostic::is_notice`]
    /// to distinguish notices, which do not count toward the status.
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone)]
pub struct LintResult {
    pub checked_files: usize,
    pub total_findings: usize,
    pub reports: Vec<LintFileReport>,
    /// `select`/`ignore` entries that name no shipped rule (likely typos).
    pub unknown_rules: Vec<String>,
}

#[derive(Debug)]
pub enum LintError {
    Discovery(FileDiscoveryError),
    Io { path: PathBuf, message: String },
}

impl std::fmt::Display for LintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LintError::Discovery(err) => write!(f, "{err}"),
            LintError::Io { path, message } => {
                write!(f, "failed to read {}: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for LintError {}

/// What the linter resolves free reads against. `SystemOnly` is the built-in
/// Base/Core snapshot with no workspace (single-file mode: `undefined-name`
/// stays opt-in and file-local). `Harvested` carries a project's harvested
/// library and derives each file's workspace membership, so cross-file names —
/// a sibling file's `using`/`import`, a same-module global — resolve exactly as
/// they do in the language server.
#[derive(Clone, Copy)]
pub enum ProjectContext<'a> {
    SystemOnly,
    Harvested(&'a crate::index::HarvestedLibrary),
    /// Explicit script programs, with the environment's library when available.
    Scripts {
        library: Option<&'a crate::index::HarvestedLibrary>,
        scripts: &'a crate::project::scripts::ScriptAnalysis,
    },
    /// The built-in snapshot plus a declared dependency set, as if the file were
    /// a source file of a package whose `Project.toml` `[deps]` list those
    /// names. The docs generator's stand-in for a real project, so
    /// `unresolved-import`'s examples render (see
    /// [`Rule::example_declared_deps`](crate::linter::rules::Rule::example_declared_deps));
    /// no discovery path produces it.
    SystemInProject(&'a DeclaredDeps),
}

impl<'a> ProjectContext<'a> {
    fn library(self) -> Option<&'a crate::index::HarvestedLibrary> {
        match self {
            Self::Harvested(library) => Some(library),
            Self::Scripts { library, .. } => library,
            _ => None,
        }
    }

    /// The [`ResolutionContext`] for the file at `path`, if any.
    ///
    /// The declared dependency set rides along only for a *package source
    /// file*: a file the harvest placed inside a workspace package, whose
    /// `using`/`import` clauses really do load against that package's own
    /// project. Everything else — a script, a `test/` file, a loose buffer —
    /// gets `None` and leaves `unresolved-import` silent.
    fn resolution_for(self, path: Option<&Path>) -> Option<ResolutionContext<'a>> {
        match self {
            ProjectContext::Scripts { library, scripts } => {
                let mut resolution = match library {
                    Some(lib) => ProjectContext::Harvested(lib).resolution_for(path)?,
                    None => ProjectContext::SystemOnly.resolution_for(path)?,
                };
                if path.is_some_and(|path| scripts.applies(path)) {
                    resolution.workspace = None;
                    resolution.declared_deps = None;
                }
                Some(resolution)
            }
            ProjectContext::SystemOnly => Some(ResolutionContext {
                packages: system_snapshot(),
                workspace: None,
                declared_deps: None,
            }),
            ProjectContext::SystemInProject(deps) => Some(ResolutionContext {
                packages: system_snapshot(),
                workspace: None,
                declared_deps: Some(Arc::new(deps.clone())),
            }),
            ProjectContext::Harvested(lib) => {
                let workspace = path.and_then(|p| lib.workspace_member(p));
                let declared_deps = workspace
                    .as_ref()
                    .and_then(|(pkg, _)| lib.declared_deps.get(&pkg.name).cloned());
                Some(ResolutionContext {
                    packages: &lib.packages,
                    workspace,
                    declared_deps,
                })
            }
        }
    }
}

struct BatchRules {
    loose: ResolvedRules,
    workspace: ResolvedRules,
    script: ResolvedRules,
}

impl BatchRules {
    fn new(
        config: &LintConfig,
        julia_target: Option<VersionRange>,
        edition: Option<SyntaxEdition>,
    ) -> (Self, Vec<String>) {
        let (loose, unknown_rules) = ResolvedRules::resolve(config);
        let configure = |rules: ResolvedRules| {
            rules
                .with_julia_target(julia_target)
                .with_syntax_edition(edition)
        };
        (
            Self {
                loose: configure(loose),
                workspace: configure(ResolvedRules::resolve_for_workspace(config).0),
                script: configure(ResolvedRules::resolve_for_scripts(config).0),
            },
            unknown_rules,
        )
    }

    fn needs_import_usage(&self) -> bool {
        [&self.loose, &self.workspace, &self.script]
            .iter()
            .any(|rules| rules.enabled().contains("unused-import"))
    }
}

#[derive(Clone, Copy)]
enum ViewRules<'a> {
    Fixed(&'a ResolvedRules),
    ByMembership(&'a BatchRules),
}

impl<'a> ViewRules<'a> {
    fn syntax_edition(self) -> Option<SyntaxEdition> {
        match self {
            Self::Fixed(rules) => rules,
            Self::ByMembership(rules) => &rules.loose,
        }
        .syntax_edition()
    }

    fn for_view(self, in_script: bool, in_workspace: bool) -> &'a ResolvedRules {
        match self {
            Self::Fixed(rules) => rules,
            Self::ByMembership(rules) if in_script => &rules.script,
            Self::ByMembership(rules) if in_workspace => &rules.workspace,
            Self::ByMembership(rules) => &rules.loose,
        }
    }
}

/// Syntax memberships and package import usage shared by a batch of lint
/// targets. Fix passes retain the original memberships and reproject changed
/// text so that candidate fixes see the current source.
pub struct LintBatch {
    rules: BatchRules,
    catalog: ContextCatalog,
    package_imports: Option<ScriptAnalysis>,
    includes: BTreeMap<ParseOptions, BTreeMap<PathBuf, Vec<IncludeProblem>>>,
    libraries: HashMap<SourceContext, Arc<HarvestedLibrary>>,
}

impl LintBatch {
    pub fn new(
        files: &[PathBuf],
        entries: &[PathBuf],
        library: Option<&crate::index::HarvestedLibrary>,
        config: &LintConfig,
        julia_target: Option<VersionRange>,
        edition: Option<SyntaxEdition>,
    ) -> (Self, Vec<String>) {
        let (rules, unknown_rules) = BatchRules::new(config, julia_target, edition);
        let packages = package_entries(library);
        let mut roots = entries.to_vec();
        roots.extend(packages.iter().cloned());
        let catalog = ContextCatalog::discover(files, &roots, &BTreeMap::new(), edition);
        let package_imports = (rules.needs_import_usage() && !packages.is_empty())
            .then(|| imports_from_catalog(&packages, &catalog));
        let mut paths_per_edition = BTreeMap::<_, Vec<_>>::new();
        let mut libraries = HashMap::new();
        let mut views = HashMap::new();
        let buffers = BTreeMap::new();
        let mut editions = DiskEditionSelections::new(&buffers);
        for path in files {
            let path = crate::incremental::normalize_path(path);
            for options in catalog.options(&path) {
                paths_per_edition
                    .entry(options)
                    .or_default()
                    .push(path.clone());
            }
            if let Some(library) = library {
                for (context, _) in catalog.selections(&path) {
                    libraries.entry(context.clone()).or_insert_with(|| {
                        let key = library.lint_view_key(context);
                        Arc::clone(views.entry(key.clone()).or_insert_with(|| {
                            Arc::new(library.lint_view(&key, edition, &mut editions))
                        }))
                    });
                }
            }
        }
        let includes = paths_per_edition
            .into_iter()
            .map(|(options, paths)| {
                (
                    options,
                    include_problems_from_catalog(&paths, options, &catalog, None),
                )
            })
            .collect();
        (
            Self {
                rules,
                catalog,
                package_imports,
                includes,
                libraries,
            },
            unknown_rules,
        )
    }

    pub fn source(&self, path: &Path) -> Result<&str, LintError> {
        self.catalog.sources[&crate::incremental::normalize_path(path)]
            .as_deref()
            .map_err(|message| LintError::Io {
                path: path.to_path_buf(),
                message: message.clone(),
            })
    }

    pub fn check(&self, path: &Path, text: &str, project: ProjectContext<'_>) -> LintFileReport {
        let key = crate::incremental::normalize_path(path);
        let selections = self.catalog.selections(&key);
        let unchanged = self
            .catalog
            .sources
            .get(&key)
            .and_then(|source| source.as_ref().ok())
            .is_some_and(|source| source.as_ref() == text);
        let updated;
        let imports = if unchanged || self.package_imports.is_none() {
            self.package_imports.as_ref()
        } else {
            updated = package_import_usage(
                Some(path),
                text,
                self.rules.loose.syntax_edition(),
                project,
                selections,
            );
            updated.as_ref()
        };
        check_text_with_imports(
            Some(path),
            text,
            ViewRules::ByMembership(&self.rules),
            project,
            selections,
            imports,
            Some((self, unchanged)),
        )
    }
}

/// Lint every `.jl` file under `paths` with default configuration.
pub fn check_paths(paths: &[PathBuf]) -> Result<LintResult, LintError> {
    check_paths_with_config(
        paths,
        &LintConfig::default(),
        &ExcludeFilter::none(),
        None,
        ProjectContext::SystemOnly,
    )
}

/// Lint every `.jl` file under `paths`, honoring `config` and `exclude`.
/// `julia_target` is the project's declared Julia support range (for the
/// `julia-version-compat` rule); `None` leaves version-gated rules silent.
pub fn check_paths_with_config(
    paths: &[PathBuf],
    config: &LintConfig,
    exclude: &ExcludeFilter,
    julia_target: Option<VersionRange>,
    project: ProjectContext<'_>,
) -> Result<LintResult, LintError> {
    check_paths_with_edition(paths, config, exclude, julia_target, project, None)
}

/// The CLI override changes source grammar independently of runtime lint bounds.
pub fn check_paths_with_edition(
    paths: &[PathBuf],
    config: &LintConfig,
    exclude: &ExcludeFilter,
    julia_target: Option<VersionRange>,
    project: ProjectContext<'_>,
    syntax_edition: Option<crate::parser::SyntaxEdition>,
) -> Result<LintResult, LintError> {
    let files = collect_julia_files(paths, exclude).map_err(LintError::Discovery)?;
    let entries = match project {
        ProjectContext::Scripts { scripts, .. } => scripts
            .programs
            .iter()
            .map(|program| program.entry.clone())
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let (batch, unknown_rules) = LintBatch::new(
        &files,
        &entries,
        project.library(),
        config,
        julia_target,
        syntax_edition,
    );
    let reports = files
        .par_iter()
        .map(|path| {
            let text = batch.source(path)?;
            Ok(batch.check(path, text, project))
        })
        .collect::<Result<Vec<_>, LintError>>()?;
    let total_findings = reports
        .iter()
        .flat_map(|report| &report.diagnostics)
        .filter(|diagnostic| !diagnostic.is_notice() && diagnostic.rule != PARSE_ERROR_RULE)
        .count();
    Ok(LintResult {
        checked_files: files.len(),
        total_findings,
        reports,
        unknown_rules,
    })
}

/// Lint an in-memory document with no path (e.g. stdin).
pub fn check_document(text: &str) -> LintFileReport {
    let (rules, _) = ResolvedRules::resolve(&LintConfig::default());
    check_text(None, text, &rules, ProjectContext::SystemOnly)
}

/// Lint `text` under `config`, attributing findings to `path`. These in-memory
/// convenience APIs use the latest syntax edition without project discovery.
/// Used by the docs generator (`crate::linter::docs`) to render each example's real diagnostics.
pub fn check_source(path: Option<&Path>, text: &str, config: &LintConfig) -> LintFileReport {
    check_source_with_target(path, text, config, None)
}

/// Like [`check_source`], but with a Julia target range for version-gated rules
/// (`julia-version-compat`). The docs generator and version-compat tests use
/// this to make the target-dependent examples fire.
pub fn check_source_with_target(
    path: Option<&Path>,
    text: &str,
    config: &LintConfig,
    julia_target: Option<VersionRange>,
) -> LintFileReport {
    check_source_in_project(path, text, config, julia_target, None)
}

/// Like [`check_source_with_target`], but linting `text` as a source file of a
/// package whose `Project.toml` `[deps]` are `declared_deps`. `None` keeps the
/// loose-file reading of [`check_source_with_target`]. The docs generator uses
/// this to render the examples of the project-gated rules
/// (`unresolved-import`), which have no findings outside a project.
pub fn check_source_in_project(
    path: Option<&Path>,
    text: &str,
    config: &LintConfig,
    julia_target: Option<VersionRange>,
    declared_deps: Option<&[&str]>,
) -> LintFileReport {
    let (rules, _) = ResolvedRules::resolve(config);
    let rules = rules.with_julia_target(julia_target);
    let deps: Option<DeclaredDeps> =
        declared_deps.map(|names| names.iter().map(|n| (*n).to_string()).collect());
    let project = match &deps {
        Some(deps) => ProjectContext::SystemInProject(deps),
        None => ProjectContext::SystemOnly,
    };
    check_text_single(
        path,
        text,
        ViewRules::Fixed(&rules),
        project,
        ParseOptions::default(),
        None,
        None,
    )
}

/// Select source contexts from project metadata, parse, follow static includes,
/// and lint. A pathless document uses the latest edition. Use `check_source`
/// when a path only attributes an in-memory example and should not select metadata.
pub fn check_text(
    path: Option<&Path>,
    text: &str,
    rules: &ResolvedRules,
    project: ProjectContext<'_>,
) -> LintFileReport {
    let mut entries = match project {
        ProjectContext::Scripts { scripts, .. } => scripts
            .programs
            .iter()
            .map(|program| program.entry.clone())
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    entries.extend(package_entries(project.library()));
    let selections;
    let catalog;
    let contexts = if let Some(path) = path {
        let path = crate::incremental::normalize_path(path);
        let buffers = BTreeMap::from([(path.clone(), Arc::from(text))]);
        catalog = ContextCatalog::discover(
            std::slice::from_ref(&path),
            &entries,
            &buffers,
            rules.syntax_edition(),
        );
        catalog.selections(&path)
    } else {
        let edition = rules.syntax_edition().unwrap_or_default();
        selections = vec![(
            SourceContext {
                entry: PathBuf::new(),
                source: EditionSource::Fixed(edition),
            },
            EditionSelection::known(edition),
        )];
        &selections
    };
    check_text_in_contexts(path, text, rules, project, contexts)
}

/// Analyze every applicable edition. Semantic rules still join the driver's
/// shared walk once per parsed view; parse errors block only their own view.
pub fn check_text_in_contexts(
    path: Option<&Path>,
    text: &str,
    rules: &ResolvedRules,
    project: ProjectContext<'_>,
    selections: &[(SourceContext, EditionSelection)],
) -> LintFileReport {
    let imports = rules
        .enabled()
        .contains("unused-import")
        .then(|| package_import_usage(path, text, rules.syntax_edition(), project, selections))
        .flatten();
    check_text_with_imports(
        path,
        text,
        ViewRules::Fixed(rules),
        project,
        selections,
        imports.as_ref(),
        None,
    )
}

fn check_text_with_imports(
    path: Option<&Path>,
    text: &str,
    rules: ViewRules<'_>,
    project: ProjectContext<'_>,
    selections: &[(SourceContext, EditionSelection)],
    package_imports: Option<&ScriptAnalysis>,
    batch: Option<(&LintBatch, bool)>,
) -> LintFileReport {
    let mut reports = Vec::new();
    let mut report_entries = Vec::new();
    let certain = selections.iter().all(|(_, selection)| selection.certain);
    let mut seen = std::collections::HashSet::new();
    for (context, selection) in selections {
        // Keep existing conservative script resolution within one environment
        // and grammar. Different metadata sources require separate library views.
        let is_script = matches!(
            context.source,
            crate::parse_context::EditionSource::Fixed(_)
                | crate::parse_context::EditionSource::Project {
                    role: crate::parse_context::SourceRole::Script,
                    ..
                }
        );
        if !seen.insert((
            context.source.clone(),
            selection.options,
            (!is_script).then_some(context.entry.clone()),
        )) {
            continue;
        }
        let filtered;
        let library_view = project.library().map(|library| {
            match batch.and_then(|(batch, _)| batch.libraries.get(context)) {
                Some(view) => Cow::Borrowed(view.as_ref()),
                None => Cow::Owned(library.in_context(context, rules.syntax_edition())),
            }
        });
        let view_project = match project {
            ProjectContext::Scripts { scripts, .. } => {
                filtered = crate::project::scripts::ScriptAnalysis {
                    programs: scripts
                        .programs
                        .iter()
                        .filter(|program| {
                            program.parse_options == selection.options
                                && selections.iter().any(|(candidate, _)| {
                                    candidate.entry == program.entry
                                        && candidate.source == context.source
                                })
                        })
                        .cloned()
                        .collect(),
                    pending: scripts.pending,
                };
                ProjectContext::Scripts {
                    library: library_view.as_deref(),
                    scripts: &filtered,
                }
            }
            ProjectContext::Harvested(_) => {
                ProjectContext::Harvested(library_view.as_deref().expect("context library"))
            }
            other => other,
        };
        let imports = package_imports.map(|usage| ScriptAnalysis {
            programs: usage
                .programs
                .iter()
                .filter(|program| {
                    program.entry == context.entry && program.parse_options == selection.options
                })
                .cloned()
                .collect(),
            pending: usage.pending,
        });
        report_entries.push(context.entry.clone());
        reports.push((
            selection.options,
            check_text_single(
                path,
                text,
                rules,
                view_project,
                selection.options,
                imports.as_ref(),
                batch,
            ),
        ));
    }
    if reports.is_empty() {
        let options = ParseOptions::default();
        reports.push((
            options,
            check_text_single(path, text, rules, project, options, package_imports, batch),
        ));
    }
    if reports.len() == 1 && certain {
        let mut report = reports.pop().unwrap().1;
        report.diagnostics.sort_by_key(diagnostic_order);
        return report;
    }
    let diagnostic_key = |d: &Diagnostic| (d.rule, d.range, d.severity, d.message.clone());
    let finding_key = |d: &Diagnostic| (d.rule, d.range, d.message.clone());
    let exact: Vec<std::collections::HashSet<_>> = reports
        .iter()
        .map(|(_, report)| report.diagnostics.iter().collect())
        .collect();
    let findings: Vec<std::collections::HashSet<_>> = reports
        .iter()
        .map(|(_, report)| report.diagnostics.iter().map(finding_key).collect())
        .collect();
    let mut merged = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (_, report) in &reports {
        for diagnostic in &report.diagnostics {
            if !seen.insert(diagnostic_key(diagnostic)) {
                continue;
            }
            let common = exact.iter().all(|report| report.contains(diagnostic));
            let mut diagnostic = diagnostic.clone();
            if !common || !certain {
                diagnostic.fixes.clear();
            }
            if !common && reports.len() > 1 {
                let key = finding_key(&diagnostic);
                let contexts = reports
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| findings[*index].contains(&key))
                    .map(|(index, (options, _))| {
                        format!(
                            "{} ({})",
                            report_entries[index].display(),
                            options.syntax_edition
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                diagnostic
                    .message
                    .body
                    .push_str(&format!(" [syntax contexts: {contexts}]"));
            }
            merged.push(diagnostic);
        }
    }
    if !certain {
        for (_, selection) in selections {
            if let Some(issue) = &selection.issue {
                let mut diagnostic = Diagnostic::new(
                    PARSE_ERROR_RULE,
                    TextRange::empty(0.into()),
                    format!(
                        "syntax edition is uncertain: {}: {}",
                        issue.path.display(),
                        issue.message
                    ),
                );
                diagnostic.severity = Severity::Error;
                diagnostic.path = path.map(Path::to_path_buf);
                if !merged.contains(&diagnostic) {
                    merged.push(diagnostic);
                }
            }
        }
    }
    if reports.len() > 1 || !certain {
        let applied = crate::linter::fix::apply_fixes(text, &merged, true);
        if reports.iter().any(|(options, _)| {
            !crate::parser::parse_with_options(&applied.output, *options)
                .diagnostics
                .is_empty()
        }) {
            for diagnostic in &mut merged {
                diagnostic.fixes.clear();
            }
        }
    }
    merged.sort_by_key(diagnostic_order);
    let parse_count = merged
        .iter()
        .filter(|diagnostic| diagnostic.rule == PARSE_ERROR_RULE)
        .count();
    let count = merged
        .iter()
        .filter(|diagnostic| !diagnostic.is_notice())
        .count();
    let status = if parse_count > 0 {
        LintStatus::ParseDiagnostics { count: parse_count }
    } else if count > 0 {
        LintStatus::Findings { count }
    } else {
        LintStatus::Clean
    };
    LintFileReport {
        path: path.map(Path::to_path_buf),
        status,
        diagnostics: merged,
    }
}

fn diagnostic_order(diagnostic: &Diagnostic) -> (rowan::TextSize, rowan::TextSize, &'static str) {
    (
        diagnostic.range.start(),
        diagnostic.range.end(),
        diagnostic.rule,
    )
}

fn check_text_single(
    path: Option<&Path>,
    text: &str,
    rules: ViewRules<'_>,
    project: ProjectContext<'_>,
    options: ParseOptions,
    import_usage: Option<&ScriptAnalysis>,
    batch: Option<(&LintBatch, bool)>,
) -> LintFileReport {
    let parsed = crate::parser::parse_with_options(text, options);
    let computed;
    let include_problems = match (path, batch) {
        // A pathless document has no base directory to resolve includes
        // against; the include-graph rules stay silent.
        (None, _) => &[][..],
        (Some(path), Some((batch, true))) => batch
            .includes
            .get(&options)
            .and_then(|problems| problems.get(&crate::incremental::normalize_path(path)))
            .map_or(&[][..], Vec::as_slice),
        (Some(path), Some((batch, false))) => {
            let path = crate::incremental::normalize_path(path);
            computed = include_problems_from_catalog(
                std::slice::from_ref(&path),
                options,
                &batch.catalog,
                Some((&path, &parsed.cst)),
            )
            .remove(&path)
            .unwrap_or_default();
            &computed
        }
        (Some(path), None) => {
            let seeds = [(path.to_path_buf(), parsed.cst.clone())];
            computed = include_problems_with_options(&seeds, options)
                .remove(path)
                .unwrap_or_default();
            &computed
        }
    };
    check_parsed(
        path,
        &parsed.cst,
        &parsed.diagnostics,
        rules,
        include_problems,
        project,
        import_usage,
        options,
    )
}

fn package_entries(library: Option<&crate::index::HarvestedLibrary>) -> Vec<PathBuf> {
    let Some(library) = library else {
        return Vec::new();
    };
    library
        .workspaces
        .iter()
        .filter_map(|name| {
            let root = library.roots.get(name)?;
            Some(crate::incremental::normalize_path(
                &root.join("src").join(format!("{name}.jl")),
            ))
        })
        .collect()
}

fn imports_from_catalog(entries: &[PathBuf], catalog: &ContextCatalog) -> ScriptAnalysis {
    let programs: std::collections::BTreeSet<_> = entries
        .iter()
        .flat_map(|entry| {
            catalog
                .selections(entry)
                .iter()
                .filter_map(|(context, selection)| {
                    (context.entry == *entry).then_some((entry.clone(), selection.options))
                })
        })
        .collect();
    ScriptAnalysis::from_sources_with_options(
        &programs.into_iter().collect::<Vec<_>>(),
        &catalog.sources,
    )
}

fn package_import_usage(
    path: Option<&Path>,
    text: &str,
    edition: Option<SyntaxEdition>,
    project: ProjectContext<'_>,
    selections: &[(SourceContext, EditionSelection)],
) -> Option<ScriptAnalysis> {
    let entries = package_entries(project.library());
    if entries.is_empty() {
        return None;
    }
    let buffers = path
        .map(|path| (crate::incremental::normalize_path(path), Arc::from(text)))
        .into_iter()
        .collect();
    let contexts: Vec<_> = selections
        .iter()
        .filter_map(|(context, _)| entries.contains(&context.entry).then_some(context.clone()))
        .collect();
    let catalog = if selections.is_empty() {
        crate::script_loading::load_contexts(&entries, &buffers, edition)
    } else {
        ContextCatalog::load(&contexts, &buffers, edition)
    };
    Some(imports_from_catalog(&entries, &catalog))
}

/// Core per-file pass over an already-parsed file: run rules on a clean tree
/// (suppressed findings are dropped inside the rules runner). Parse-error
/// pseudo-diagnostics short-circuit before rules or suppression run, so
/// `parse-error` is not suppressible — by design.
#[allow(clippy::too_many_arguments)]
fn check_parsed(
    path: Option<&Path>,
    root: &SyntaxNode,
    parse_diagnostics: &[crate::parser::ParseDiagnostic],
    rules: ViewRules<'_>,
    includes: &[IncludeProblem],
    project: ProjectContext<'_>,
    import_usage: Option<&crate::project::scripts::ScriptAnalysis>,
    options: crate::parser::ParseOptions,
) -> LintFileReport {
    if !parse_diagnostics.is_empty() {
        let diagnostics = parse_diagnostics
            .iter()
            .map(|diag| Diagnostic {
                rule: PARSE_ERROR_RULE,
                severity: Severity::Error,
                path: path.map(Path::to_path_buf),
                range: TextRange::new((diag.start as u32).into(), (diag.end as u32).into()),
                message: ViolationData::new(PARSE_ERROR_RULE, diag.message.clone()),
                fixes: Vec::new(),
            })
            .collect();
        return LintFileReport {
            path: path.map(Path::to_path_buf),
            status: LintStatus::ParseDiagnostics {
                count: parse_diagnostics.len(),
            },
            diagnostics,
        };
    }

    let model = SemanticModel::build(root);
    // The resolution the caller chose: the built-in Base/Core snapshot with no
    // workspace (single-file mode — deterministic and cheap, and why
    // `undefined-name` is opt-in there), or a harvested project library plus
    // this file's workspace membership, which resolves cross-file names.
    let resolution = project.resolution_for(path);
    let import_usage = import_usage.filter(|usage| {
        resolution.as_ref().is_some_and(|r| r.workspace.is_some())
            || path.is_some_and(|path| usage.applies(path))
    });
    let scripts = match project {
        ProjectContext::Scripts { scripts, .. } => Some(scripts),
        _ => None,
    };
    let rules = rules.for_view(
        path.is_some_and(|path| scripts.is_some_and(|scripts| scripts.applies(path))),
        resolution
            .as_ref()
            .is_some_and(|resolution| resolution.workspace.is_some()),
    );
    let diagnostics = lint_parsed_with_import_usage(
        path,
        root,
        &model,
        rules,
        resolution,
        includes,
        scripts,
        import_usage,
        options,
    );

    let count = diagnostics.iter().filter(|d| !d.is_notice()).count();
    let status = if count == 0 {
        LintStatus::Clean
    } else {
        LintStatus::Findings { count }
    };

    LintFileReport {
        path: path.map(Path::to_path_buf),
        status,
        diagnostics,
    }
}

/// The built-in Base/Core export snapshot, built once per process. The
/// resolution floor for CLI lint runs (and docs generation), where locating
/// and harvesting a real Julia install would be slow and nondeterministic.
pub(crate) fn system_snapshot() -> &'static BTreeMap<String, Arc<PackageIndex>> {
    static SNAPSHOT: OnceLock<BTreeMap<String, Arc<PackageIndex>>> = OnceLock::new();
    SNAPSHOT.get_or_init(|| build_system_index(None))
}

/// Run `rules` against an already-parsed *clean* tree (rules need one; the
/// caller is responsible for gating on parse diagnostics). The file's
/// `# fatou-ignore` directives are parsed off the CST here; the suppressed
/// findings are dropped inside [`ResolvedRules::run`], which needs the
/// directive list on the [`RuleContext`] anyway. Shared by [`check_text`] and
/// the language server, whose warm path lints off the salsa-cached tree and
/// model instead of re-parsing (and passes its own harvested library as the
/// `resolution` context).
pub fn lint_parsed(
    path: Option<&Path>,
    root: &SyntaxNode,
    model: &SemanticModel,
    rules: &ResolvedRules,
    resolution: Option<ResolutionContext<'_>>,
    includes: &[IncludeProblem],
) -> Vec<Diagnostic> {
    lint_parsed_with_scripts(path, root, model, rules, resolution, includes, None)
}

/// Project-aware linting without adding another rule traversal or suppression pass.
#[allow(clippy::too_many_arguments)]
pub fn lint_parsed_with_scripts(
    path: Option<&Path>,
    root: &SyntaxNode,
    model: &SemanticModel,
    rules: &ResolvedRules,
    resolution: Option<ResolutionContext<'_>>,
    includes: &[IncludeProblem],
    scripts: Option<&crate::project::scripts::ScriptAnalysis>,
) -> Vec<Diagnostic> {
    lint_parsed_with_import_usage(
        path,
        root,
        model,
        rules,
        resolution,
        includes,
        scripts,
        None,
        crate::parser::ParseOptions::default(),
    )
}

/// Package include usage shares the project model without changing resolution
/// contexts or enabling script defaults.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lint_parsed_with_import_usage(
    path: Option<&Path>,
    root: &SyntaxNode,
    model: &SemanticModel,
    rules: &ResolvedRules,
    resolution: Option<ResolutionContext<'_>>,
    includes: &[IncludeProblem],
    scripts: Option<&crate::project::scripts::ScriptAnalysis>,
    import_usage: Option<&crate::project::scripts::ScriptAnalysis>,
    options: crate::parser::ParseOptions,
) -> Vec<Diagnostic> {
    let suppressions = SuppressionMap::build(root);
    let ctx = RuleContext::new(path, root, model)
        .with_parse_options(options)
        .with_resolution(resolution)
        .with_scripts(scripts)
        .with_import_usage(import_usage)
        .with_includes(includes)
        .with_julia_target(rules.julia_target())
        .with_config(rules.rules_config())
        .with_suppressions(&suppressions)
        .with_enabled_rules(rules.enabled());
    rules.run(&ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_file_reports_clean() {
        let report = check_document("x = 1\n");
        assert_eq!(report.status, LintStatus::Clean);
    }

    #[test]
    fn parse_diagnostics_are_surfaced() {
        let report = check_document("f (x)\n");
        assert!(matches!(
            report.status,
            LintStatus::ParseDiagnostics { count } if count > 0
        ));
        assert_eq!(report.diagnostics.len(), 1);
        let diag = &report.diagnostics[0];
        assert_eq!(diag.rule, PARSE_ERROR_RULE);
        assert_eq!(diag.severity, Severity::Error);
        assert!(!diag.message.body.is_empty());
    }
}
