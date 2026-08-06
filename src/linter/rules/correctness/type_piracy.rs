//! `type-piracy`: a method definition that extends a function the current
//! module does not own, with no argument type it owns either.
//!
//! Julia dispatches on a global method table, so adding a method to *someone
//! else's* function using only *someone else's* types silently changes
//! behavior for every other package that shares those types — the definition
//! is visible the moment your module loads. This is "type piracy". A method is
//! non-pirating as long as you own at least one of the two ingredients: the
//! generic function, or one of the argument types (a type parameter counts).
//!
//! What counts as *owned* is answered by the shared name-resolution masking
//! order in [`crate::resolve::Resolver`], so the rule agrees with
//! `undefined-name`, completion, and hover:
//!
//! - a **local/file binding** that is a definition (a `struct` here, a
//!   function defined here) or a **workspace sibling** (`Resolution::Workspace`)
//!   is owned;
//! - a name bound by `import`/`using` ([`BindingKind::Import`] or
//!   `Resolution::WorkspaceImport`), a whole-module `using`'s export
//!   (`Resolution::Using`), or a Base/Core name (`Resolution::System`) is
//!   *foreign* — importing a type does not make you its owner;
//! - anything the resolver cannot place (`Resolution::Unresolved`, an
//!   interpolated type, an exotic type expression) is *unknown*.
//!
//! Soundness comes first — this is a correctness rule, so a false positive is
//! worse than a miss. The rule flags a definition only when it can *positively*
//! prove the function is foreign **and** every argument type it can read is
//! foreign. It withholds (reports nothing for that definition) the moment any
//! ingredient is unknown, and it skips the whole file when a whole-module
//! `using` cannot be resolved (it might re-export an owned name). Definitions
//! inside quoted code or a macro call are skipped too: a macro may rewrite the
//! signature, so its written shape is not trustworthy.
//!
//! `where` type variables are not types you own: `f(::T) where {T}` owns
//! nothing, but a *bound* does — `f(::T) where {T <: MyType}` owns `MyType`.
//! Untyped positional arguments are `Any` (a Base type), so they never rescue a
//! definition from being pirating.
//!
//! Off by default: like `undefined-name`, the rule needs project context to be
//! sound. The language server enables it for workspace member files; on the CLI
//! it resolves against the built-in Base/Core snapshot and is opt-in via
//! `--select`.

use std::collections::HashSet;

use rowan::TextSize;

use crate::ast::{AssignmentExpr, AstNode, AstToken, FunctionDef};
use crate::index::harvest::callee_name;
use crate::index::typeexpr::dotted_path;
use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::resolve::{Namespace, PackageSource, Resolution, Resolver};
use crate::semantic::signature::{annotation_parts, peel_signature};
use crate::semantic::{BindingKind, SemanticModel};
use crate::syntax::{SyntaxKind, SyntaxNode};

pub struct TypePiracy;

/// Whether a resolved name is owned by the current module, foreign to it, or
/// of unknown provenance.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ownership {
    Owned,
    Foreign,
    Unknown,
}

/// Classify a [`Resolution`] as [`Ownership`]. An in-file binding is owned
/// unless it was introduced by `import`/`using` (an import binds a foreign
/// name); a workspace sibling is owned; every other resolved tier is foreign;
/// an unresolved name is unknown.
fn classify(res: &Resolution, model: &SemanticModel) -> Ownership {
    match res {
        Resolution::Binding(id) => {
            if model.binding(*id).kind == BindingKind::Import {
                Ownership::Foreign
            } else {
                Ownership::Owned
            }
        }
        Resolution::Workspace { .. } => Ownership::Owned,
        Resolution::WorkspaceImport { .. }
        | Resolution::Using { .. }
        | Resolution::System { .. } => Ownership::Foreign,
        Resolution::Unresolved => Ownership::Unknown,
    }
}

impl Rule for TypePiracy {
    fn id(&self) -> &'static str {
        "type-piracy"
    }

    fn default_enabled(&self) -> bool {
        // Sound only with project context: without a resolved library the rule
        // cannot tell an owned type from a foreign one. The language server
        // enables it for workspace member files; the CLI leaves it opt-in.
        false
    }

    fn description(&self) -> &'static str {
        "Flag a method definition that extends a function the current module \
         does not own, using only argument types it does not own either \
         (\"type piracy\"). Because Julia dispatches on one global method \
         table, such a method silently changes behavior for every other user \
         of those types the moment the module loads. A definition is fine as \
         long as it owns the function or at least one argument type (a type \
         parameter or `where` bound counts). The rule is sound-first: it flags \
         only when it can prove the function and every readable argument type \
         are foreign, withholding on anything unknown, and it skips the whole \
         file when a whole-module `using` cannot be resolved. Off by default: \
         it needs project context, so the language server enables it for \
         workspace member files while the CLI leaves it opt-in via `--select`."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "Extending `Base.show` for only Base types is piracy:",
            source: "Base.show(io::IO, x::Int) = print(io, x)\n",
        }]
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(resolver) = ctx.resolver() else {
            return;
        };
        // A whole-module `using` the library cannot resolve may re-export an
        // owned name, which would make a "foreign" verdict unsound.
        if ctx.has_unresolvable_using() {
            return;
        }
        for node in ctx.root.descendants() {
            let signature = match node.kind() {
                SyntaxKind::FUNCTION_DEF => FunctionDef::cast(node.clone())
                    .and_then(|def| def.signature())
                    .and_then(|sig| sig.expr())
                    .map(|expr| expr.syntax().clone()),
                // A short-form definition is a plain `=` whose left side is a
                // signature; `+=` and friends carry a different operator.
                SyntaxKind::ASSIGNMENT_EXPR => {
                    let assign = AssignmentExpr::cast(node.clone());
                    let is_eq = assign
                        .as_ref()
                        .and_then(AssignmentExpr::op)
                        .is_some_and(|op| op.syntax().kind() == SyntaxKind::EQ);
                    is_eq
                        .then(|| assign.and_then(|a| a.lhs()))
                        .flatten()
                        .map(|lhs| lhs.syntax().clone())
                }
                _ => continue,
            };
            let Some(signature) = signature else { continue };
            // A quoted or macro-wrapped definition may be data or reshaped by
            // the macro; its written signature is not trustworthy.
            if node.ancestors().any(|a| {
                matches!(
                    a.kind(),
                    SyntaxKind::QUOTE_EXPR | SyntaxKind::QUOTE_SYM | SyntaxKind::MACRO_CALL
                )
            }) {
                continue;
            }
            self.check_definition(&signature, resolver, ctx.model, sink);
        }
    }
}

impl TypePiracy {
    fn check_definition<P: PackageSource + ?Sized>(
        &self,
        signature: &SyntaxNode,
        resolver: &Resolver<'_, P>,
        model: &SemanticModel,
        sink: &mut Vec<Diagnostic>,
    ) {
        let (core, wheres, _return_ty) = peel_signature(signature.clone());
        let Some(core) = core.filter(|c| c.kind() == SyntaxKind::CALL_EXPR) else {
            return;
        };
        let Some((name, owner, name_range)) = callee_name(&core) else {
            return;
        };

        // The function is foreign unless we can prove otherwise. For a bare
        // name resolve it at its definition site (catches an `import`ed name);
        // for a qualified `Mod.f` resolve the head qualifier `Mod`.
        let func_ownership = match &owner {
            None => classify(
                &resolver.resolve(&name, name_range.start(), Namespace::Value),
                model,
            ),
            Some(path) => match path.first() {
                Some(head) => classify(
                    &resolver.resolve(head, core.text_range().start(), Namespace::Value),
                    model,
                ),
                None => return,
            },
        };
        match func_ownership {
            // You own the function, or we cannot prove it is foreign.
            Ownership::Owned | Ownership::Unknown => return,
            Ownership::Foreign => {}
        }

        // Collect the `where` type variables (excluded from ownership) and the
        // bound expressions (which *do* count: `where {T <: MyType}` owns
        // `MyType`).
        let mut typevars = HashSet::new();
        let mut type_nodes: Vec<SyntaxNode> = Vec::new();
        for spec in &wheres {
            collect_where_spec(spec, &mut typevars, &mut type_nodes);
        }

        // Collect the positional argument types (keyword parameters do not
        // participate in dispatch, so they cannot make a method non-pirating).
        if let Some(arg_list) = core.children().find(|c| c.kind() == SyntaxKind::ARG_LIST) {
            for arg in arg_list.children().filter(|c| c.kind() == SyntaxKind::ARG) {
                let annotation = arg
                    .descendants()
                    .find(|d| d.kind() == SyntaxKind::TYPE_ANNOTATION);
                if let Some(annotation) = annotation {
                    let (_pattern, types) = annotation_parts(&annotation);
                    type_nodes.extend(types);
                }
            }
        }

        // Scan every type reference. Owning any of them clears the definition;
        // any unknown reference withholds the finding.
        let mut refs = Vec::new();
        let mut unknown = false;
        for node in &type_nodes {
            collect_type_refs(node, &typevars, &mut refs, &mut unknown);
        }
        for (name, offset) in refs {
            match classify(&resolver.resolve(&name, offset, Namespace::Value), model) {
                Ownership::Owned => return,
                Ownership::Unknown => unknown = true,
                Ownership::Foreign => {}
            }
        }
        if unknown {
            return;
        }

        let display = match &owner {
            Some(path) => format!("{}.{}", path.join("."), name),
            None => name.clone(),
        };
        sink.push(Diagnostic::new(
            self.id(),
            name_range,
            format!(
                "`{display}` commits type piracy: it extends a function this \
                 module does not own, and no argument type is owned here either"
            ),
        ));
    }
}

/// Split a `where` spec into the type variable it introduces (added to
/// `typevars`) and the bound expressions it constrains it by (pushed to
/// `bounds`). Descends braced/argument groups (`where {T, S <: Real}`).
fn collect_where_spec(
    node: &SyntaxNode,
    typevars: &mut HashSet<String>,
    bounds: &mut Vec<SyntaxNode>,
) {
    match node.kind() {
        SyntaxKind::BRACES | SyntaxKind::ARG | SyntaxKind::ARG_LIST => {
            for child in node.children() {
                collect_where_spec(&child, typevars, bounds);
            }
        }
        SyntaxKind::NAME | SyntaxKind::NONSTANDARD_IDENTIFIER => {
            if let Some(name) = name_text(node) {
                typevars.insert(name);
            }
        }
        // `T <: Upper` / `T >: Lower`: the variable, then the bound.
        SyntaxKind::BINARY_EXPR => {
            let mut children = node.children();
            if let Some(var) = children.next()
                && let Some(name) = name_text(&var)
            {
                typevars.insert(name);
            }
            bounds.extend(children);
        }
        // `Lower <: T <: Upper`: the middle operand is the variable.
        SyntaxKind::COMPARISON_EXPR => {
            let parts: Vec<SyntaxNode> = node.children().collect();
            if parts.len() == 3 {
                if let Some(name) = name_text(&parts[1]) {
                    typevars.insert(name);
                }
                bounds.push(parts[0].clone());
                bounds.push(parts[2].clone());
            }
        }
        _ => {}
    }
}

/// Collect every type name a type-position expression references, as
/// `(head_name, offset)` pairs to resolve. A `where` type variable is skipped.
/// A value parameter (a literal or symbol in `Foo{2}`/`Val{:x}`) contributes
/// nothing; an interpolation or otherwise unreadable shape sets `unknown`.
fn collect_type_refs(
    node: &SyntaxNode,
    typevars: &HashSet<String>,
    refs: &mut Vec<(String, TextSize)>,
    unknown: &mut bool,
) {
    match node.kind() {
        SyntaxKind::NAME | SyntaxKind::NONSTANDARD_IDENTIFIER => {
            if let Some(name) = name_text(node) {
                if typevars.contains(&name) {
                    return;
                }
                refs.push((name, node.text_range().start()));
            }
        }
        // A type application `Foo{A, B}`: the base and each type argument.
        SyntaxKind::CURLY_EXPR => {
            if let Some(base) = node.children().next() {
                collect_type_refs(&base, typevars, refs, unknown);
            }
            for arg in node
                .children()
                .filter(|c| c.kind() == SyntaxKind::ARG_LIST)
                .flat_map(|list| list.children())
            {
                let inner = if arg.kind() == SyntaxKind::ARG {
                    arg.children().next()
                } else {
                    Some(arg)
                };
                if let Some(inner) = inner {
                    collect_type_refs(&inner, typevars, refs, unknown);
                }
            }
        }
        SyntaxKind::PAREN_EXPR => {
            if let Some(inner) = node.children().next() {
                collect_type_refs(&inner, typevars, refs, unknown);
            }
        }
        // A qualified type name `Base.AbstractDict`: ownership is decided by the
        // head qualifier, which begins at the node's start.
        SyntaxKind::BINARY_EXPR | SyntaxKind::COMPARISON_EXPR => match dotted_path(node) {
            Some(path) => {
                if let Some(head) = path.into_iter().next() {
                    refs.push((head, node.text_range().start()));
                }
            }
            None => *unknown = true,
        },
        // A value parameter, not a type: contributes no ownership.
        SyntaxKind::LITERAL
        | SyntaxKind::STRING_LITERAL
        | SyntaxKind::CMD_LITERAL
        | SyntaxKind::QUOTE_SYM
        | SyntaxKind::QUOTE_EXPR
        | SyntaxKind::UNARY_EXPR => {}
        // An interpolated or otherwise unreadable type: provenance unknown.
        _ => *unknown = true,
    }
}

/// The identifier text of a `NAME` or `var"..."` node.
fn name_text(node: &SyntaxNode) -> Option<String> {
    let token = node.children_with_tokens().filter_map(|el| el.into_token());
    match node.kind() {
        SyntaxKind::NAME => token
            .filter(|t| t.kind() == SyntaxKind::IDENT)
            .map(|t| t.text().to_string())
            .next(),
        SyntaxKind::NONSTANDARD_IDENTIFIER => token
            .filter(|t| t.kind() == SyntaxKind::STRING_CONTENT)
            .map(|t| t.text().to_string())
            .next(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::model::{
        DefLocation, ExportedName, ModuleIndex, PackageIndex, Span, TypeDef, TypeKind, Visibility,
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

    /// A library with a `Base` exporting `exports`.
    fn base(exports: &[&str]) -> BTreeMap<String, Arc<PackageIndex>> {
        let pkg = PackageIndex {
            name: "Base".to_string(),
            root: ModuleIndex {
                name: "Base".to_string(),
                bare: false,
                loc: loc(),
                exports: exports
                    .iter()
                    .map(|n| ExportedName {
                        name: n.to_string(),
                        visibility: Visibility::Exported,
                        loc: loc(),
                    })
                    .collect(),
                functions: Vec::new(),
                types: Vec::new(),
                consts: Vec::new(),
                macros: Vec::new(),
                submodules: Vec::new(),
                usings: Vec::new(),
                imported_names: Vec::new(),
            },
            members: Vec::new(),
            member_modules: Default::default(),
            diagnostics: Vec::new(),
        };
        BTreeMap::from([("Base".to_string(), Arc::new(pkg))])
    }

    /// A workspace package `MyPkg` defining top-level `types`, and binding the
    /// module-level imported `imported` names (a sibling file's load surface).
    fn workspace(types: &[&str], imported: &[&str]) -> Arc<PackageIndex> {
        Arc::new(PackageIndex {
            name: "MyPkg".to_string(),
            root: ModuleIndex {
                name: "MyPkg".to_string(),
                bare: false,
                loc: loc(),
                exports: Vec::new(),
                functions: Vec::new(),
                types: types
                    .iter()
                    .map(|t| TypeDef {
                        name: t.to_string(),
                        kind: TypeKind::Abstract,
                        type_params: Vec::new(),
                        supertype: None,
                        fields: Vec::new(),
                        doc: None,
                        loc: loc(),
                    })
                    .collect(),
                consts: Vec::new(),
                macros: Vec::new(),
                submodules: Vec::new(),
                usings: Vec::new(),
                imported_names: imported.iter().map(|n| n.to_string()).collect(),
            },
            members: Vec::new(),
            member_modules: Default::default(),
            diagnostics: Vec::new(),
        })
    }

    /// Lint `src` with the rule alone against `packages` and a workspace.
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
            }));
        let mut sink = Vec::new();
        TypePiracy.check_file(&ctx, &mut sink);
        sink.into_iter().map(|d| d.message.body).collect()
    }

    #[test]
    fn workspace_owned_type_is_not_piracy() {
        // `MyType` is defined in a sibling file of the package, so extending
        // `Base.show` for it is legitimate.
        let lib = base(&["Base", "IO", "show"]);
        let ws = workspace(&["MyType"], &[]);
        assert_eq!(
            messages("Base.show(io::IO, x::MyType) = 0\n", &lib, Some(ws)),
            Vec::<String>::new(),
        );
    }

    #[test]
    fn workspace_imported_type_is_still_piracy() {
        // A sibling's `import Foo: Bar` binds `Bar`, but importing a type does
        // not make you its owner: extending `Base.show` for it is piracy.
        let lib = base(&["Base", "IO", "show"]);
        let ws = workspace(&[], &["Bar"]);
        let msgs = messages("Base.show(io::IO, x::Bar) = 0\n", &lib, Some(ws));
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(msgs[0].contains("Base.show"), "{msgs:?}");
    }
}
