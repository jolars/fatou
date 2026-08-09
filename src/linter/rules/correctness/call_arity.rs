//! `call-arity`: a call no visible method of the function can accept.
//!
//! A call whose positional count falls outside every method's accepted range,
//! or that passes a keyword no positionally-matching method declares, raises
//! `MethodError` (or `UndefKeywordError`) the moment it runs. The method
//! table is assembled from the same tiers name resolution walks: a fresh
//! harvest of the file's own tree (so same-file definitions stay current
//! between saves), the enclosing workspace package's index, and — for names
//! that resolve to a `using`'d export or Base/Core — the harvested library.
//!
//! Julia's open method tables make "which methods exist?" undecidable for a
//! file in isolation, so every unknown *weakens* findings instead of
//! inventing them. The method set is an over-approximation: all same-name
//! groups in each consulted tree are unioned, qualified extensions
//! (`Base.show(io, x) = ...`) included, so a method the rule cannot place
//! precisely still clears the call. On the CLI's baked-in Base/Core snapshot
//! (names without signatures) library calls simply find no methods and stay
//! silent.
//!
//! The whole file is skipped when it calls `eval`/`@eval`, `include`s
//! anything without a workspace context (or with a dynamic path), or
//! `using`s a module the library cannot resolve — in each case methods may
//! exist that the model cannot see. A single call is skipped when it splats
//! positional arguments, carries a `do` block (an invisible leading function
//! argument), sits inside a macro call or quoted code, targets anything but
//! a bare function name (constructors, callable values, qualified and
//! operator callees), or reaches a group with a bodyless `function f end`
//! placeholder (methods live elsewhere by design).
//!
//! Off by default, like `undefined-name`: a bare file may be an `include`d
//! fragment whose siblings add methods. The language server enables the rule
//! for workspace member files; on the CLI it is opt-in via `--select`, sound
//! for self-contained scripts.

use crate::ast::AstToken;
use crate::index::harvest_tree;
use crate::index::model::{Method, ModuleIndex, PackageIndex};
use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::matchers::{self, CallShape};
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::resolve::{Namespace, Resolution};
use crate::semantic::{BindingKind, LoadKind};
use crate::syntax::SyntaxKind;
use std::sync::Arc;

pub struct CallArity;

/// Names every module defines implicitly. Their per-module methods are
/// invisible to the harvest, so a library group would misjudge them.
const MODULE_IMPLICIT: &[&str] = &["eval", "include", "new", "ccall"];

impl Rule for CallArity {
    fn id(&self) -> &'static str {
        "call-arity"
    }

    fn default_enabled(&self) -> bool {
        // Sound only with project context: an `include`d fragment's siblings
        // may add methods to any function defined here. The language server
        // turns the rule on for workspace member files; CLI users opt in for
        // self-contained scripts.
        false
    }

    fn description(&self) -> &'static str {
        "Flag a call that no visible method of the function accepts: a \
         positional count outside every method's range, or a keyword argument \
         no positionally-matching method declares. Such a call raises \
         `MethodError` at runtime. The method table unions every tier name \
         resolution sees — the file's own definitions, the workspace package, \
         and the harvested library, qualified extensions included — so an \
         unknown method always silences the check rather than triggering it. \
         Calls that splat arguments, carry `do` blocks, sit in macro calls or \
         quoted code, or target constructors and callable values are exempt, \
         and a file that `eval`s or `include`s outside a known workspace is \
         skipped entirely. Off by default: the rule needs project context to \
         be sound, so the language server enables it for workspace member \
         files, while the CLI leaves it opt-in for self-contained scripts."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "`half` has no two-argument method:",
            source: "half(x) = x / 2\n\nhalf(3, 4)\n",
        }]
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        // No resolution context, an unresolvable whole-module `using`, `eval`,
        // or an unfollowable `include`: all four leave the file unanswerable
        // (see `RuleContext::trusts_resolution`).
        if !ctx.trusts_resolution() {
            return;
        }
        let (Some(resolution), Some(resolver)) = (&ctx.resolution, ctx.resolver()) else {
            return;
        };

        let scan = ctx.file_scan();
        let file_index = harvest_tree(ctx.root);
        let workspace_root = resolution.workspace.as_ref().map(|(pkg, _)| &pkg.root);
        let library = library_packages(ctx, resolution.packages);

        for node in ctx.root.descendants() {
            if node.kind() != SyntaxKind::CALL_EXPR {
                continue;
            }
            if scan.in_skipped(node.text_range()) {
                continue;
            }
            // A definition's signature is a `CALL_EXPR` too — declaring
            // parameters, not passing arguments.
            let Some(call) = matchers::call_expr(&node) else {
                continue;
            };
            let Some(ident) = call.callee_ident() else {
                continue;
            };
            let name = ident.text();
            if MODULE_IMPLICIT.contains(&name) {
                continue;
            }
            let site = CallShape::of(&call);
            // A positional splat leaves the count unknown; a `do` block passes
            // a leading function argument the argument list does not show.
            if site.positional_open || site.do_block {
                continue;
            }

            // The trees whose same-name groups may hold methods of this call's
            // target, per resolution tier. A masked library function stays out
            // of a file-resolved name's table, so local shadows keep their
            // own arity.
            let offset = node.text_range().start();
            let mut roots: Vec<&ModuleIndex> = Vec::new();
            match resolver.resolve(name, offset, Namespace::Value) {
                Resolution::Binding(id) => {
                    let binding = ctx.model.binding(id);
                    if binding.kind != BindingKind::Function {
                        continue;
                    }
                    // A local `function` (a closure) never reaches a harvest.
                    if !ctx.model.scope(binding.scope).kind.is_global() {
                        continue;
                    }
                }
                Resolution::Workspace { .. } => {}
                Resolution::Using { .. } | Resolution::System { .. } => {
                    roots.extend(library.iter().map(|pkg| &pkg.root));
                }
                // A sibling file's module-level import names an external
                // function whose source module we do not record; skip arity.
                Resolution::WorkspaceImport { .. } => continue,
                Resolution::Unresolved => continue, // `undefined-name`'s business
            }
            roots.push(&file_index);
            roots.extend(workspace_root);

            let mut table = MethodTable::default();
            for root in roots {
                table.collect(root, name);
            }
            // A same-named type means constructors (implicit ones invisible
            // here); a bodyless `function f end` announces methods defined
            // elsewhere. Either way the table is not the whole story.
            if table.methods.is_empty() || table.saw_type || table.saw_placeholder {
                continue;
            }

            let arities: Vec<Arity> = table.methods.iter().map(Arity::of).collect();
            let matching: Vec<&Arity> = arities
                .iter()
                .filter(|a| a.admits(site.positional.len()))
                .collect();
            if matching.is_empty() {
                let count = site.positional.len();
                let plural = if count == 1 { "" } else { "s" };
                let accepted = render_accepted(&arities);
                sink.push(Diagnostic::new(
                    self.id(),
                    node.text_range(),
                    format!(
                        "no method of `{name}` takes {count} positional \
                         argument{plural} (methods accept {accepted})"
                    ),
                ));
                continue;
            }
            if site.keyword_open {
                continue;
            }
            for keyword in &site.keywords {
                let kw = keyword.name.text();
                let unknown = matching
                    .iter()
                    .all(|a| !a.kw_open && !a.kws.iter().any(|k| k == kw));
                if unknown {
                    sink.push(Diagnostic::new(
                        self.id(),
                        keyword.name.syntax().text_range(),
                        format!(
                            "no matching method of `{name}` accepts the keyword argument `{kw}`"
                        ),
                    ));
                }
            }
        }
    }
}

/// The harvested packages whose trees may carry methods of a library-resolved
/// name: Base and Core (the implicit tier) plus every whole-module `using`'d
/// package — any of them may extend any function.
fn library_packages(
    ctx: &RuleContext<'_>,
    packages: &dyn crate::resolve::PackageSource,
) -> Vec<Arc<PackageIndex>> {
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    let mut add = |name: &str, out: &mut Vec<Arc<PackageIndex>>| {
        if seen.iter().any(|n| n == name) {
            return;
        }
        seen.push(name.to_string());
        if let Some(pkg) = packages.package(name) {
            out.push(pkg);
        }
    };
    add("Base", &mut out);
    add("Core", &mut out);
    for load in ctx.model.module_loads() {
        if load.kind != LoadKind::Using || load.items.is_some() {
            continue;
        }
        if let Some(first) = load.path.components.first() {
            add(first, &mut out);
        }
    }
    out
}

/// The unioned method set for one name, with the guards that void it.
#[derive(Default)]
struct MethodTable {
    methods: Vec<Method>,
    /// A same-named type exists: the call may be a constructor, whose
    /// implicit and inner methods the harvest does not record.
    saw_type: bool,
    /// A bodyless `function f end` exists: methods live elsewhere by design.
    saw_placeholder: bool,
}

impl MethodTable {
    /// Union every same-name group in `module`'s tree, owners included — a
    /// qualified extension in any nested module adds a method globally.
    fn collect(&mut self, module: &ModuleIndex, name: &str) {
        for group in module.functions.iter().filter(|g| g.name == name) {
            for method in &group.methods {
                if method.has_body {
                    self.methods.push(method.clone());
                } else {
                    self.saw_placeholder = true;
                }
            }
        }
        if module.types.iter().any(|t| t.name == name) {
            self.saw_type = true;
        }
        for sub in &module.submodules {
            self.collect(sub, name);
        }
    }
}

/// One method's accepted shape: the positional range and the keyword names.
struct Arity {
    min: usize,
    /// `None` for a vararg method (no upper bound).
    max: Option<usize>,
    kws: Vec<String>,
    /// Accepts any keyword: a `kwargs...` slurp or an unnamed keyword
    /// parameter the harvest could not name.
    kw_open: bool,
}

impl Arity {
    fn of(method: &Method) -> Self {
        let min = method
            .params
            .iter()
            .take_while(|p| p.default.is_none() && !p.is_vararg)
            .count();
        let max = if method.params.iter().any(|p| p.is_vararg) {
            None
        } else {
            Some(method.params.len())
        };
        let kw_open = method
            .keyword_params
            .iter()
            .any(|p| p.is_vararg || p.name.is_none());
        let kws = method
            .keyword_params
            .iter()
            .filter_map(|p| p.name.clone())
            .collect();
        Arity {
            min,
            max,
            kws,
            kw_open,
        }
    }

    fn admits(&self, positional: usize) -> bool {
        self.min <= positional && self.max.is_none_or(|max| positional <= max)
    }
}

/// Render the accepted positional counts compactly: `1`, `1-2`, `2+`, merged
/// across methods and joined with commas.
fn render_accepted(arities: &[Arity]) -> String {
    let mut ranges: Vec<(usize, Option<usize>)> = arities.iter().map(|a| (a.min, a.max)).collect();
    ranges.sort_by_key(|r| (r.0, r.1.is_none(), r.1));
    let mut merged: Vec<(usize, Option<usize>)> = Vec::new();
    for (min, max) in ranges {
        match merged.last_mut() {
            // Extend the open or adjacent/overlapping previous range.
            Some((_, prev_max)) if prev_max.is_none_or(|pm| min <= pm + 1) => {
                *prev_max = match (*prev_max, max) {
                    (None, _) | (_, None) => None,
                    (Some(a), Some(b)) => Some(a.max(b)),
                };
            }
            _ => merged.push((min, max)),
        }
    }
    let parts: Vec<String> = merged
        .into_iter()
        .map(|(min, max)| match max {
            None => format!("{min}+"),
            Some(max) if max == min => format!("{min}"),
            Some(max) => format!("{min}-{max}"),
        })
        .collect();
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::model::{
        DefLocation, ExportedName, FunctionGroup, ModuleIndex, PackageIndex, Param, Span,
        Visibility,
    };
    use crate::linter::rules::ResolutionContext;
    use crate::semantic::SemanticModel;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn loc() -> DefLocation {
        DefLocation {
            file: "src/x.jl".into(),
            range: Span { start: 0, end: 0 },
        }
    }

    fn positional(n: usize) -> Vec<Param> {
        (0..n)
            .map(|i| Param {
                name: Some(format!("x{i}")),
                ..Param::default()
            })
            .collect()
    }

    fn method(params: Vec<Param>) -> Method {
        Method {
            params,
            keyword_params: Vec::new(),
            type_args: Vec::new(),
            where_clauses: Vec::new(),
            return_type: None,
            has_body: true,
            doc: None,
            loc: loc(),
        }
    }

    fn module_named(name: &str) -> ModuleIndex {
        ModuleIndex {
            name: name.to_string(),
            bare: false,
            loc: loc(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            consts: Vec::new(),
            macros: Vec::new(),
            submodules: Vec::new(),
            usings: Vec::new(),
            imported_names: Vec::new(),
        }
    }

    /// A Base index exporting `name` with one `arity`-positional method.
    fn base_with(name: &str, arity: usize) -> BTreeMap<String, Arc<PackageIndex>> {
        let mut root = module_named("Base");
        root.exports.push(ExportedName {
            name: name.to_string(),
            visibility: Visibility::Exported,
            loc: loc(),
        });
        root.functions.push(FunctionGroup {
            name: name.to_string(),
            owner: None,
            methods: vec![method(positional(arity))],
            doc: None,
        });
        let pkg = PackageIndex {
            name: "Base".to_string(),
            root,
            members: Vec::new(),
            member_modules: Default::default(),
            diagnostics: Vec::new(),
        };
        BTreeMap::from([("Base".to_string(), Arc::new(pkg))])
    }

    /// A workspace package whose root holds `groups`.
    fn workspace(groups: Vec<FunctionGroup>) -> Arc<PackageIndex> {
        let mut root = module_named("MyPkg");
        root.functions = groups;
        Arc::new(PackageIndex {
            name: "MyPkg".to_string(),
            root,
            members: Vec::new(),
            member_modules: Default::default(),
            diagnostics: Vec::new(),
        })
    }

    fn messages(
        src: &str,
        packages: &BTreeMap<String, Arc<PackageIndex>>,
        ws: Option<Arc<PackageIndex>>,
    ) -> Vec<String> {
        let parsed = crate::parser::parse(src);
        assert!(parsed.diagnostics.is_empty(), "fixture must parse clean");
        let model = SemanticModel::build(&parsed.cst);
        let ctx =
            RuleContext::new(None, &parsed.cst, &model).with_resolution(Some(ResolutionContext {
                packages,
                workspace: ws.map(|pkg| (pkg, Vec::new())),
                declared_deps: None,
            }));
        let mut sink = Vec::new();
        CallArity.check_file(&ctx, &mut sink);
        sink.into_iter().map(|d| d.message.body).collect()
    }

    #[test]
    fn base_call_is_checked_against_harvested_signatures() {
        let lib = base_with("clamp", 3);
        let msgs = messages("clamp(1, 2)\n", &lib, None);
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(msgs[0].contains("clamp"), "{msgs:?}");
        assert_eq!(
            messages("clamp(1, 2, 3)\n", &lib, None),
            Vec::<String>::new()
        );
    }

    #[test]
    fn workspace_sibling_methods_are_seen() {
        let lib = base_with("clamp", 3);
        let ws = workspace(vec![FunctionGroup {
            name: "helper".to_string(),
            owner: None,
            methods: vec![method(positional(2))],
            doc: None,
        }]);
        let msgs = messages("helper(1)\n", &lib, Some(ws.clone()));
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert_eq!(
            messages("helper(1, 2)\n", &lib, Some(ws)),
            Vec::<String>::new()
        );
    }

    #[test]
    fn workspace_extension_of_a_base_function_clears_the_call() {
        // The workspace adds a two-argument method to Base's three-argument
        // `clamp` via a qualified extension; the union admits both counts.
        let lib = base_with("clamp", 3);
        let ws = workspace(vec![FunctionGroup {
            name: "clamp".to_string(),
            owner: Some(vec!["Base".to_string()]),
            methods: vec![method(positional(2))],
            doc: None,
        }]);
        assert_eq!(
            messages("clamp(1, 2)\n", &lib, Some(ws)),
            Vec::<String>::new()
        );
    }

    #[test]
    fn same_file_definitions_mask_the_library_group() {
        // The file's own one-argument `clamp` masks Base's three-argument
        // one, so the two-argument call has no admitting method.
        let lib = base_with("clamp", 3);
        let msgs = messages("clamp(x) = x\nclamp(1, 2)\n", &lib, None);
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(msgs[0].contains("accept 1"), "{msgs:?}");
    }

    #[test]
    fn a_generator_argument_counts_as_one_positional() {
        // The lone-generator call carries no argument list, and a generator
        // sharing one is not wrapped in an `ARG`; either way it is exactly one
        // positional argument.
        let lib = base_with("minimum", 1);
        assert_eq!(
            messages("minimum(g(x) for x in group)\n", &lib, None),
            Vec::<String>::new()
        );

        let lib = base_with("reduce", 2);
        assert_eq!(
            messages("reduce(+, x for x in xs)\n", &lib, None),
            Vec::<String>::new()
        );
        // Counted, not waved through: a genuinely wrong count still reports.
        let msgs = messages("reduce(x for x in xs)\n", &lib, None);
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(msgs[0].contains("takes 1 positional argument"), "{msgs:?}");
    }

    #[test]
    fn no_resolution_context_is_silent() {
        let parsed = crate::parser::parse("f(x) = x\nf(1, 2)\n");
        let model = SemanticModel::build(&parsed.cst);
        let ctx = RuleContext::new(None, &parsed.cst, &model);
        let mut sink = Vec::new();
        CallArity.check_file(&ctx, &mut sink);
        assert!(sink.is_empty());
    }

    #[test]
    fn accepted_ranges_render_compactly() {
        let a = |min, max| Arity {
            min,
            max,
            kws: Vec::new(),
            kw_open: false,
        };
        assert_eq!(render_accepted(&[a(1, Some(1))]), "1");
        assert_eq!(render_accepted(&[a(1, Some(2))]), "1-2");
        assert_eq!(render_accepted(&[a(2, None)]), "2+");
        // Adjacent ranges merge; a gap stays split.
        assert_eq!(render_accepted(&[a(1, Some(1)), a(2, Some(3))]), "1-3");
        assert_eq!(render_accepted(&[a(0, Some(0)), a(2, Some(2))]), "0, 2");
        assert_eq!(render_accepted(&[a(1, Some(1)), a(1, None)]), "1+");
    }
}
