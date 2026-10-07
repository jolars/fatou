//! Explicit script programs. Graph construction uses range-free projections;
//! disk loading is a separate adapter, also used by the LSP's background worker.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::ast::{AstNode, AstToken, CallExpr, MacroCall};
use crate::incremental::normalize_path;
use crate::index::ModuleUsing;
use crate::resolve::{ModulePath, PackageSource, module_path_unresolvable};
use crate::semantic::{BindingKind, LoadKind, SemanticModel};
use crate::syntax::SyntaxNode;
use smol_str::SmolStr;

use super::{IncludeEdge, include_edges, include_target};

/// Definitions whose values cannot be inferred from a static include closure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DefinitionEffects {
    pub calls_eval: bool,
    pub literal_include: bool,
    pub dynamic_include: bool,
}

impl DefinitionEffects {
    /// Record definition effects and report whether this node is an include.
    pub(crate) fn observe(&mut self, node: &SyntaxNode) -> bool {
        if let Some(call) = MacroCall::cast(node.clone()) {
            if call
                .name()
                .and_then(|name| name.macro_token())
                .is_some_and(|t| t.text() == "eval")
            {
                self.calls_eval = true;
            }
        } else if let Some(call) = CallExpr::cast(node.clone()) {
            if let Some(callee) = call.callee_ident() {
                match callee.text() {
                    "eval" => self.calls_eval = true,
                    "include" if include_target(&call).is_some() => {
                        self.literal_include = true;
                        return true;
                    }
                    "include" => {
                        self.dynamic_include = true;
                        return true;
                    }
                    _ => {}
                }
            } else if let Some(callee) = call.callee()
                && let Some(token) = callee.syntax().last_token()
                && matches!(token.text(), "include" | "eval")
            {
                // Qualified and computed include/eval calls have no supported
                // static target; treating them as absent would invent findings.
                self.dynamic_include = true;
                return token.text() == "include";
            }
        }
        false
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScriptModule {
    pub bindings: BTreeSet<SmolStr>,
    pub usings: Vec<ModuleUsing>,
}

/// A file's contribution, independent of byte offsets and function bodies.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScriptFile {
    pub modules: BTreeMap<ModulePath, ScriptModule>,
    pub includes: Vec<IncludeEdge>,
    pub incomplete: bool,
    events: Vec<ScriptEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ScriptEvent {
    Using(ModulePath, ModuleUsing),
    Include(IncludeEdge),
}

impl ScriptFile {
    pub fn project(
        root: &SyntaxNode,
        model: &SemanticModel,
        path: &Path,
        parse_clean: bool,
    ) -> Self {
        let mut out = Self {
            includes: include_edges(root, path.parent()),
            incomplete: !parse_clean,
            ..Self::default()
        };
        let mut events = Vec::new();
        for (site, edge) in super::include_sites(root).into_iter().zip(&out.includes) {
            events.push((site.call.start(), ScriptEvent::Include(edge.clone())));
        }
        for binding in model.bindings() {
            if model.scope(binding.scope).kind.is_global() {
                let name = if binding.kind == BindingKind::Macro {
                    format!("@{}", binding.name).into()
                } else {
                    binding.name.clone()
                };
                out.modules
                    .entry(model.enclosing_module_path(binding.scope))
                    .or_default()
                    .bindings
                    .insert(name);
            }
        }
        for load in model.module_loads() {
            if load.kind == LoadKind::Using && load.items.is_none() {
                events.push((
                    load.range.start(),
                    ScriptEvent::Using(
                        model.enclosing_module_path(load.scope),
                        ModuleUsing {
                            leading_dots: load.path.leading_dots,
                            components: load
                                .path
                                .components
                                .iter()
                                .map(ToString::to_string)
                                .collect(),
                        },
                    ),
                ));
            }
        }
        events.sort_by_key(|(offset, _)| *offset);
        out.events = events.into_iter().map(|(_, event)| event).collect();
        let mut effects = DefinitionEffects::default();
        for node in root.descendants() {
            effects.observe(&node);
        }
        out.incomplete |= effects.calls_eval || effects.dynamic_include;
        out
    }

    pub fn parse(path: &Path, text: &str) -> Self {
        let parsed = crate::parser::parse(text);
        Self::project(
            &parsed.cst,
            &SemanticModel::build(&parsed.cst),
            path,
            parsed.diagnostics.is_empty(),
        )
    }
}

/// A source that has loaded, failed to load, or has not been attempted yet.
#[derive(Debug, Clone)]
pub(crate) enum ScriptFileState {
    Available(Arc<ScriptFile>),
    Failed,
    Pending,
}

/// One program's globals. Membership deliberately includes the host module.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScriptProgram {
    pub entry: PathBuf,
    pub modules: BTreeMap<ModulePath, ScriptModule>,
    pub members: BTreeMap<PathBuf, BTreeSet<ModulePath>>,
    pub incomplete: bool,
    /// Confirmed failed reads, keyed by the file containing each include.
    /// Paths are decoded literals, so consumers recover spans from current text.
    pub failed_includes: BTreeMap<PathBuf, BTreeSet<String>>,
}

impl ScriptProgram {
    /// Build from known projections. An absent projection leaves analysis
    /// incomplete, without asserting that a filesystem read has failed.
    pub fn build(entry: &Path, mut file: impl FnMut(&Path) -> Option<Arc<ScriptFile>>) -> Self {
        Self::build_with_availability(entry, |path| match file(path) {
            Some(file) => ScriptFileState::Available(file),
            None => ScriptFileState::Pending,
        })
    }

    pub(crate) fn build_with_availability(
        entry: &Path,
        mut file: impl FnMut(&Path) -> ScriptFileState,
    ) -> Self {
        let mut out = Self {
            entry: normalize_path(entry),
            ..Self::default()
        };
        let mut active = BTreeSet::new();
        out.visit(&normalize_path(entry), &Vec::new(), &mut file, &mut active);
        out
    }

    fn visit(
        &mut self,
        path: &Path,
        host: &ModulePath,
        file: &mut impl FnMut(&Path) -> ScriptFileState,
        active: &mut BTreeSet<PathBuf>,
    ) -> bool {
        if active.contains(path) {
            self.incomplete = true;
            return true;
        }
        let first_visit = self
            .members
            .entry(path.to_path_buf())
            .or_default()
            .insert(host.clone());
        // Check availability even on repeated visits: every include of a failed
        // source needs its own diagnostic, including diamond-shaped graphs.
        let projection = match file(path) {
            ScriptFileState::Available(projection) => projection,
            ScriptFileState::Failed => {
                self.incomplete = true;
                return false;
            }
            ScriptFileState::Pending => {
                self.incomplete = true;
                return true;
            }
        };
        if !first_visit {
            return true;
        }
        active.insert(path.to_path_buf());
        self.incomplete |= projection.incomplete;
        for (suffix, module) in &projection.modules {
            let path: ModulePath = host.iter().chain(suffix).cloned().collect();
            let dest = self.modules.entry(path).or_default();
            dest.bindings.extend(module.bindings.iter().cloned());
            dest.usings.extend(module.usings.iter().cloned());
        }
        for event in &projection.events {
            let edge = match event {
                ScriptEvent::Using(suffix, using) => {
                    let path = host.iter().chain(suffix).cloned().collect();
                    self.modules
                        .entry(path)
                        .or_default()
                        .usings
                        .push(using.clone());
                    continue;
                }
                ScriptEvent::Include(edge) => edge,
            };
            let Some(target) = &edge.target else {
                self.incomplete = true;
                continue;
            };
            let mut child_host = host.clone();
            child_host.extend(edge.host_suffix.iter().map(SmolStr::new));
            if !self.visit(&normalize_path(target), &child_host, file, active) {
                self.failed_includes
                    .entry(path.to_path_buf())
                    .or_default()
                    .insert(edge.path.clone());
            }
        }
        active.remove(path);
        true
    }

    pub fn complete(&self, packages: &dyn PackageSource) -> bool {
        !self.incomplete
            && self.modules.values().all(|module| {
                module
                    .usings
                    .iter()
                    .all(|u| !module_path_unresolvable(u.leading_dots, &u.components, packages))
            })
    }
}

#[derive(Clone)]
pub struct ScriptContext<'a> {
    pub program: &'a ScriptProgram,
    pub host: ModulePath,
}

impl ScriptContext<'_> {
    pub fn label(&self) -> String {
        let module = std::iter::once("Main")
            .chain(self.host.iter().map(SmolStr::as_str))
            .collect::<Vec<_>>()
            .join(".");
        format!("{} ({module})", self.program.entry.display())
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScriptAnalysis {
    pub programs: Vec<Arc<ScriptProgram>>,
    /// Loading is not evidence that an unknown name is absent.
    pub pending: bool,
}

impl ScriptAnalysis {
    /// Confirmed failures only; an unfinished load is not evidence of absence.
    pub fn failed_includes(&self) -> BTreeMap<PathBuf, BTreeSet<String>> {
        let mut failures = BTreeMap::<_, BTreeSet<_>>::new();
        if !self.pending {
            for program in &self.programs {
                for (from, paths) in &program.failed_includes {
                    failures
                        .entry(from.clone())
                        .or_default()
                        .extend(paths.iter().cloned());
                }
            }
        }
        failures
    }

    pub fn contexts(&self, path: &Path) -> Vec<ScriptContext<'_>> {
        let path = normalize_path(path);
        self.programs
            .iter()
            .flat_map(|program| {
                program
                    .members
                    .get(&path)
                    .into_iter()
                    .flatten()
                    .map(|host| ScriptContext {
                        program,
                        host: host.clone(),
                    })
            })
            .collect()
    }

    pub fn applies(&self, path: &Path) -> bool {
        // An incomplete entry may have lost the includes that establish this
        // file's membership. Falling back to standalone analysis would turn
        // unavailable context into spurious undefined-name diagnostics.
        self.pending
            || self.programs.iter().any(|program| {
                program.incomplete || program.members.contains_key(&normalize_path(path))
            })
    }

    pub fn from_sources(entries: &[PathBuf], sources: &ScriptSources) -> Self {
        let files: BTreeMap<_, _> = sources
            .iter()
            .map(|(path, text)| {
                let state = match text {
                    Ok(text) => ScriptFileState::Available(Arc::new(ScriptFile::parse(path, text))),
                    Err(_) => ScriptFileState::Failed,
                };
                (path.clone(), state)
            })
            .collect();
        Self {
            programs: entries
                .iter()
                .map(|entry| {
                    Arc::new(ScriptProgram::build_with_availability(entry, |path| {
                        files.get(path).cloned().unwrap_or(ScriptFileState::Pending)
                    }))
                })
                .collect(),
            pending: false,
        }
    }
}

pub type ScriptSources = BTreeMap<PathBuf, Result<Arc<str>, String>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{PackageIndex, harvest_tree};
    use crate::resolve::{Namespace, Resolution, Resolver};

    #[test]
    fn using_order_follows_includes_and_disagreeing_contexts_are_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.jl");
        let second = dir.path().join("second.jl");
        let helper = dir.path().join("helper.jl");
        let source = "f() = shared\n";
        let sources = BTreeMap::from([
            (
                first.clone(),
                Ok(Arc::from(
                    "include(\"imports.jl\")\nusing B\ninclude(\"helper.jl\")\n",
                )),
            ),
            (
                second.clone(),
                Ok(Arc::from(
                    "using B\ninclude(\"imports.jl\")\ninclude(\"helper.jl\")\n",
                )),
            ),
            (dir.path().join("imports.jl"), Ok(Arc::from("using A\n"))),
            (helper.clone(), Ok(Arc::from(source))),
        ]);
        let scripts = ScriptAnalysis::from_sources(&[first, second], &sources);
        let library: BTreeMap<_, _> = ["A", "B"]
            .into_iter()
            .map(|name| {
                let mut root = harvest_tree(&crate::parser::parse("export shared\n").cst);
                root.name = name.into();
                (
                    name.to_string(),
                    Arc::new(PackageIndex {
                        name: name.into(),
                        root,
                        members: Vec::new(),
                        member_modules: BTreeMap::new(),
                        diagnostics: Vec::new(),
                    }),
                )
            })
            .collect();
        let parsed = crate::parser::parse(source);
        let model = SemanticModel::build(&parsed.cst);
        let contexts = scripts.contexts(&helper);
        let resolver = Resolver::new(&model, &library).with_scripts(contexts.clone());
        let offset = (source.find("shared").unwrap() as u32).into();
        for (context, expected) in contexts.iter().zip(["A", "B"]) {
            assert!(context.program.complete(&library));
            assert_eq!(
                resolver.resolve_in_script("shared", offset, Namespace::Value, context),
                Resolution::Using {
                    module: expected.into(),
                    name: "shared".into()
                }
            );
        }
        assert_eq!(
            resolver.resolve("shared", offset, Namespace::Value),
            Resolution::Ambiguous
        );
    }
}
