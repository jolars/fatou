//! `function-has-no-methods`: a call to a function that has none.
//!
//! `function f end` declares a function with an empty method table — a
//! forward reference, so that a name exists before its methods do (or so a
//! docstring has something to attach to). Calling it before any method is
//! defined raises `MethodError` unconditionally, whatever the arguments.
//!
//! The rule is [`call-arity`](super::CallArity)'s callee resolution one step
//! further: where that rule *bails* on a group holding a bodyless
//! placeholder — methods may live elsewhere by design — this one asks whether
//! anywhere is anywhere at all. It fires only when the union of every method
//! table the file can see holds nothing but placeholders: the file's own
//! definitions (a fresh harvest of its tree, so same-file methods stay
//! current between saves) plus the enclosing workspace package's index.
//!
//! Because Julia's method tables are open, the *closed world* is the gate. A
//! name that resolves to a `using`'d export or Base/Core is never checked: the
//! package that owns it may add methods in a file no harvest here covers.
//! Even inside the workspace two shapes stay exempt:
//!
//! - a declaration the package `export`s or declares `public` — an interface
//!   hook a package extension (`ext/`, outside the harvest's include closure)
//!   or a downstream package is meant to fill in, which is what a bare
//!   `function f end` most often announces;
//! - a name that also names a type, whose constructors the harvest does not
//!   record.
//!
//! The whole-file bail-outs are [`call-arity`](super::CallArity)'s, via
//! [`RuleContext::trusts_resolution`]: `eval`/`@eval` (the `@eval`-in-a-loop
//! method factory among them), an `include` no harvest can follow, and an
//! unresolvable `using`. A call site inside a macro call or quoted code is
//! skipped too.
//!
//! Off by default, like `call-arity`: outside a workspace a file may be an
//! `include`d fragment whose siblings define the methods. The language server
//! enables it for workspace member files; on the CLI it is opt-in via
//! `--select`.

use crate::ast::AstToken;
use crate::index::harvest_tree;
use crate::index::model::ModuleIndex;
use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::matchers;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::resolve::{Namespace, Resolution};
use crate::semantic::BindingKind;
use crate::syntax::SyntaxKind;

pub struct FunctionHasNoMethods;

impl Rule for FunctionHasNoMethods {
    fn id(&self) -> &'static str {
        "function-has-no-methods"
    }

    fn default_enabled(&self) -> bool {
        // Sound only with project context: an `include`d fragment's siblings
        // may define the methods this file cannot see. The language server
        // turns the rule on for workspace member files; CLI users opt in.
        false
    }

    fn description(&self) -> &'static str {
        "Flag a call to a function whose every visible definition is a bodyless \
         `function f end` declaration. Such a function has an empty method \
         table, so the call raises `MethodError` whatever it passes. Only a \
         name the closed world defines is checked — one belonging to this \
         file or to the enclosing workspace package — since the owner of a \
         `using`'d or Base/Core name may add methods elsewhere. A declaration \
         the package `export`s or declares `public` is exempt as an interface \
         hook for a package extension or a downstream package to implement, \
         as is a name that also names a type. The file is skipped entirely \
         when it `eval`s, `include`s outside a known workspace, or `using`s a \
         module the library cannot resolve, and call sites in macro calls or \
         quoted code are exempt. Off by default: like `call-arity` the rule \
         needs project context to be sound, so the language server enables it \
         for workspace member files while the CLI leaves it opt-in."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "`normalize` is declared but never given a method:",
            source: "function normalize end\n\nnormalize(\"data\")\n",
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

        for node in ctx.root.descendants() {
            if node.kind() != SyntaxKind::CALL_EXPR {
                continue;
            }
            if scan.in_skipped(node.text_range()) {
                continue;
            }
            // A definition's signature is a `CALL_EXPR` too — declaring
            // parameters, not passing arguments. (It is also what makes the
            // function have a method.)
            let Some(call) = matchers::call_expr(&node) else {
                continue;
            };
            let Some(ident) = call.callee_ident() else {
                continue;
            };
            let name = ident.text();

            // Only the closed world: a library name's owner may define methods
            // in a file no harvest here covers, and a sibling file's import
            // names an external function whose source module we do not record.
            match resolver.resolve(name, node.text_range().start(), Namespace::Value) {
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
                _ => continue,
            }

            let mut table = DeclarationScan::default();
            table.collect(&file_index, name);
            if let Some(root) = workspace_root {
                table.collect(root, name);
            }
            if !table.saw_placeholder || table.saw_method || table.saw_type || table.declared_api {
                continue;
            }

            sink.push(Diagnostic::new(
                self.id(),
                ident.syntax().text_range(),
                format!(
                    "`{name}` has no methods: every definition is a bodyless \
                     `function {name} end`"
                ),
            ));
        }
    }
}

/// What the consulted trees say about one name: whether a bare declaration
/// exists, and whether anything else does that would make the call fine — or
/// unanswerable.
#[derive(Default)]
struct DeclarationScan {
    /// A bodyless `function f end` exists.
    saw_placeholder: bool,
    /// A method with a body exists: the call has something to dispatch to.
    saw_method: bool,
    /// A same-named type exists: the call may be a constructor, whose implicit
    /// and inner methods the harvest does not record.
    saw_type: bool,
    /// The name is `export`ed or declared `public`: an interface hook others
    /// are meant to implement.
    declared_api: bool,
}

impl DeclarationScan {
    /// Union every same-name group in `module`'s tree, owners included — a
    /// qualified extension in any nested module adds a method globally.
    fn collect(&mut self, module: &ModuleIndex, name: &str) {
        for group in module.functions.iter().filter(|g| g.name == name) {
            for method in &group.methods {
                if method.has_body {
                    self.saw_method = true;
                } else {
                    self.saw_placeholder = true;
                }
            }
        }
        if module.types.iter().any(|t| t.name == name) {
            self.saw_type = true;
        }
        if module.exports.iter().any(|e| e.name == name) {
            self.declared_api = true;
        }
        for sub in &module.submodules {
            self.collect(sub, name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::model::{
        DefLocation, FunctionGroup, Method, ModuleIndex, PackageIndex, Span,
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

    fn method(has_body: bool) -> Method {
        Method {
            params: Vec::new(),
            keyword_params: Vec::new(),
            type_args: Vec::new(),
            where_clauses: Vec::new(),
            return_type: None,
            has_body,
            doc: None,
            loc: loc(),
        }
    }

    /// A workspace package whose root holds one group for `name`.
    fn workspace(name: &str, methods: Vec<Method>) -> Arc<PackageIndex> {
        let root = ModuleIndex {
            name: "MyPkg".to_string(),
            bare: false,
            loc: loc(),
            exports: Vec::new(),
            functions: vec![FunctionGroup {
                name: name.to_string(),
                owner: None,
                methods,
                doc: None,
            }],
            types: Vec::new(),
            consts: Vec::new(),
            macros: Vec::new(),
            submodules: Vec::new(),
            usings: Vec::new(),
            imported_names: Vec::new(),
        };
        Arc::new(PackageIndex {
            name: "MyPkg".to_string(),
            root,
            members: Vec::new(),
            member_modules: Default::default(),
            diagnostics: Vec::new(),
        })
    }

    fn messages(src: &str, ws: Option<Arc<PackageIndex>>) -> Vec<String> {
        let parsed = crate::parser::parse(src);
        assert!(parsed.diagnostics.is_empty(), "fixture must parse clean");
        let model = SemanticModel::build(&parsed.cst);
        let packages: BTreeMap<String, Arc<PackageIndex>> = BTreeMap::new();
        let ctx =
            RuleContext::new(None, &parsed.cst, &model).with_resolution(Some(ResolutionContext {
                packages: &packages,
                workspace: ws.map(|pkg| (pkg, Vec::new())),
                declared_deps: None,
            }));
        let mut sink = Vec::new();
        FunctionHasNoMethods.check_file(&ctx, &mut sink);
        sink.into_iter().map(|d| d.message.body).collect()
    }

    #[test]
    fn a_workspace_declaration_with_no_methods_is_flagged() {
        // The name resolves through the workspace tier: a sibling file
        // declares `helper` and nothing in the package defines a method.
        let ws = workspace("helper", vec![method(false)]);
        let msgs = messages("helper(1)\n", Some(ws));
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(msgs[0].contains("helper"), "{msgs:?}");
    }

    #[test]
    fn a_workspace_method_elsewhere_clears_the_call() {
        // The declaration is a forward reference; a sibling file's method
        // fills it in.
        let ws = workspace("helper", vec![method(false), method(true)]);
        assert_eq!(messages("helper(1)\n", Some(ws)), Vec::<String>::new());
    }

    #[test]
    fn the_file_supplies_the_missing_method() {
        // The workspace index lags an unsaved buffer, so the file's own fresh
        // harvest has to count.
        let ws = workspace("helper", vec![method(false)]);
        assert_eq!(
            messages("helper(x) = x\nhelper(1)\n", Some(ws)),
            Vec::<String>::new()
        );
    }

    #[test]
    fn no_resolution_context_is_silent() {
        let parsed = crate::parser::parse("function f end\nf(1)\n");
        let model = SemanticModel::build(&parsed.cst);
        let ctx = RuleContext::new(None, &parsed.cst, &model);
        let mut sink = Vec::new();
        FunctionHasNoMethods.check_file(&ctx, &mut sink);
        assert!(sink.is_empty());
    }
}
