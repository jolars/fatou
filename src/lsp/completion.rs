//! Completion (`textDocument/completion` and `completionItem/resolve`).
//!
//! Three contexts, decided by a lexical backward scan from the cursor (robust to
//! the parser's error recovery on the partial input completion always sees, like
//! `Foo.` or `@t`):
//!
//! - **value** — a bare identifier: every name visible at the cursor in the
//!   shared masking order ([`Resolver::visible`]), plus Julia's keywords;
//! - **macro** — the run starts with `@`: the visible names in the macro
//!   namespace, each keeping its `@`;
//! - **member** — the run follows a dotted receiver (`Foo.`, `A.B.`): every name
//!   *defined* in the resolved library module (Julia qualified access reaches
//!   non-exported names too), so functions, types, consts, macros, and
//!   submodules.
//!
//! Docstrings and full signatures are filled lazily in [`resolve_completion`]
//! (`completionItem/resolve`) from the [`data`](CompletionItem::data) key each
//! library item carries, so the initial list stays cheap.
//!
//! The receiver of a member access resolves to a harvested module only
//! (`Base.`, `LinearAlgebra.`, nested `A.B.`); value and type receivers are out
//! of scope until there is type inference.

use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::Arc;

use lsp_types::{
    CompletionItem, CompletionItemKind, Documentation, MarkupContent, MarkupKind, Position,
};
use rowan::TextSize;
use serde::{Deserialize, Serialize};

use crate::incremental::Analysis;
use crate::index::{ModuleIndex, PackageIndex};
use crate::parser::{KEYWORDS, parse};
use crate::resolve::{Candidate, Namespace, PackageSource, Resolver, Source, resolve_submodule};
use crate::semantic::{BindingKind, SemanticModel};
use crate::text::{LineIndex, PositionEncoding};

use super::render::{binding_detail, function_detail, type_detail};

/// The lazy-resolve payload stashed on each library-sourced item: the module it
/// came from (package name first, then any submodule chain) and the name to look
/// up there. A macro keeps its `@` in `name`, which selects the macro table in
/// [`resolve_completion`].
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResolveData {
    module_path: Vec<String>,
    name: String,
}

/// The completion items for `text` at `position`, re-parsing it. Pure and
/// unit-testable; `packages` supplies the library (Base/Core and loaded
/// packages) the value and member contexts draw on.
pub fn compute_completions<P: PackageSource>(
    text: &str,
    position: Position,
    encoding: PositionEncoding,
    packages: &P,
) -> Vec<CompletionItem> {
    let offset = LineIndex::new(text).position_to_byte(position, encoding);
    let model = SemanticModel::build(&parse(text).cst);
    // The pure path has no file path to key workspace membership on; the live
    // server passes the workspace module through `completion_via_db`.
    completions_for(&model, packages, None, text, TextSize::new(offset as u32))
}

/// Compute completions off the snapshot's cached parse when the db's tracked
/// buffer for `path` still matches `text`; otherwise re-parse. A write racing
/// the read trips `salsa::Cancelled`, which also falls back to a fresh parse.
/// Mirrors [`document_symbols_via_db`](super::symbols::document_symbols_via_db).
pub(crate) fn completion_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &str,
    position: Position,
    encoding: PositionEncoding,
) -> Vec<CompletionItem> {
    let offset = TextSize::new(LineIndex::new(text).position_to_byte(position, encoding) as u32);
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            // The tracked input lags the live buffer; the cached model is stale.
            return None;
        }
        let model = snapshot.semantic_model(file);
        let workspace = snapshot.workspace_module(path);
        Some(completions_for(model, snapshot, workspace, text, offset))
    }));
    match cached {
        Ok(Some(items)) => items,
        // Cache miss (`Ok(None)`) or a racing write (`Err`): re-parse from text.
        Ok(None) | Err(_) => compute_completions(text, position, encoding, snapshot),
    }
}

/// Fill an item's `documentation` (and, for a function, its `detail` signature)
/// from the harvested library, keyed by the item's [`ResolveData`]. Returns the
/// item unchanged when it carries no key or the symbol has no docs.
pub(crate) fn resolve_completion(snapshot: &Analysis, item: CompletionItem) -> CompletionItem {
    resolve_completion_with(
        &|name| snapshot.library_package(name).map(|p| (*p).clone()),
        item,
    )
}

/// The masking-order candidate set for `text` at `offset`, mapped to LSP items.
fn completions_for<P: PackageSource>(
    model: &SemanticModel,
    packages: &P,
    workspace: Option<Arc<PackageIndex>>,
    text: &str,
    offset: TextSize,
) -> Vec<CompletionItem> {
    match context_at(text, offset.into()) {
        Context::Member {
            receiver,
            macro_member,
        } => member_completions(packages, &receiver, macro_member),
        Context::Macro => Resolver::new(model, packages)
            .with_workspace(workspace)
            .visible(offset, Namespace::Macro)
            .into_iter()
            .map(|c| candidate_item(model, c, Namespace::Macro))
            .collect(),
        Context::Value => {
            let mut items: Vec<CompletionItem> = Resolver::new(model, packages)
                .with_workspace(workspace)
                .visible(offset, Namespace::Value)
                .into_iter()
                .map(|c| candidate_item(model, c, Namespace::Value))
                .collect();
            items.extend(KEYWORDS.iter().map(|kw| keyword_item(kw)));
            items
        }
    }
}

// --- context detection -----------------------------------------------------

/// What the cursor is completing, decided by the text just before it.
#[derive(Debug, PartialEq, Eq)]
enum Context {
    Value,
    Macro,
    /// After a dotted receiver: `receiver` is the module path (`A.B.` →
    /// `["A", "B"]`), `macro_member` is true for `Foo.@` (a macro member).
    Member {
        receiver: Vec<String>,
        macro_member: bool,
    },
}

/// Classify the cursor at byte `offset` by scanning the identifier run and the
/// punctuation just before it.
fn context_at(text: &str, offset: usize) -> Context {
    let prefix = &text[..offset.min(text.len())];
    let (_word, rest) = take_ident_back(prefix);
    let (macro_sigil, rest) = match rest.strip_suffix('@') {
        Some(r) => (true, r),
        None => (false, rest),
    };
    if let Some(before_dot) = rest.strip_suffix('.') {
        let receiver = scan_dotted(before_dot);
        if !receiver.is_empty() {
            return Context::Member {
                receiver,
                macro_member: macro_sigil,
            };
        }
    }
    if macro_sigil {
        Context::Macro
    } else {
        Context::Value
    }
}

/// Split off the trailing identifier run of `prefix`, returning `(run, before)`.
/// The run is empty (and `before` is all of `prefix`) when `prefix` does not end
/// in an identifier character.
fn take_ident_back(prefix: &str) -> (&str, &str) {
    let start = prefix
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_ident_char(*c))
        .last()
        .map(|(i, _)| i)
        .unwrap_or(prefix.len());
    (&prefix[start..], &prefix[..start])
}

/// The dotted module path ending `s` (`"A.B"` → `["A", "B"]`), or empty when the
/// text before the dot is not a chain of identifiers.
fn scan_dotted(s: &str) -> Vec<String> {
    let mut comps = Vec::new();
    let mut cursor = s;
    loop {
        let (ident, rest) = take_ident_back(cursor);
        if ident.is_empty() {
            break;
        }
        comps.push(ident.to_string());
        match rest.strip_suffix('.') {
            Some(r) => cursor = r,
            None => break,
        }
    }
    comps.reverse();
    comps
}

/// Whether `c` can appear inside a Julia identifier. Approximate — good enough
/// to delimit the completion context, not to lex.
fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '!'
}

// --- item construction -----------------------------------------------------

/// Turn a visible [`Candidate`] into a completion item. A file binding gets a
/// precise kind from its [`BindingKind`]; a library name is classified by
/// convention (see [`heuristic_kind`]) and carries a lazy-resolve key.
fn candidate_item(model: &SemanticModel, cand: Candidate, ns: Namespace) -> CompletionItem {
    let label = cand.name.to_string();
    match cand.source {
        Source::Binding(id) => {
            let kind = model.binding(id).kind;
            CompletionItem {
                label,
                kind: Some(binding_kind(kind)),
                detail: Some(binding_detail(kind).to_string()),
                ..Default::default()
            }
        }
        // A workspace sibling lives in the library map under its package name,
        // so it resolves lazily through the same key as any library item.
        Source::Workspace { module } | Source::Using { module } | Source::System { module } => {
            library_item(label, &[module.to_string()], ns)
        }
    }
}

/// An item for a library name (a `using` export or a Base/Core name): a
/// convention-based kind, the source module as `detail`, and a resolve key.
fn library_item(name: String, module_path: &[String], ns: Namespace) -> CompletionItem {
    let detail = module_path.last().cloned();
    CompletionItem {
        label: name.clone(),
        kind: Some(heuristic_kind(&name, ns)),
        detail,
        data: resolve_data(module_path, &name),
        ..Default::default()
    }
}

fn keyword_item(kw: &str) -> CompletionItem {
    CompletionItem {
        label: kw.to_string(),
        kind: Some(CompletionItemKind::KEYWORD),
        ..Default::default()
    }
}

/// The lazy-resolve key for looking `name` up in the module at `module_path`.
fn resolve_data(module_path: &[String], name: &str) -> Option<serde_json::Value> {
    serde_json::to_value(ResolveData {
        module_path: module_path.to_vec(),
        name: name.to_string(),
    })
    .ok()
}

/// Map a file binding's kind to an LSP completion kind.
fn binding_kind(kind: BindingKind) -> CompletionItemKind {
    use BindingKind::*;
    match kind {
        Global | Local | ForVar | LetVar => CompletionItemKind::VARIABLE,
        Const => CompletionItemKind::CONSTANT,
        Param | KeywordParam | CatchParam => CompletionItemKind::VARIABLE,
        TypeParam => CompletionItemKind::TYPE_PARAMETER,
        Field => CompletionItemKind::FIELD,
        Function => CompletionItemKind::FUNCTION,
        // LSP has no macro kind; match the document-symbol convention.
        Macro => CompletionItemKind::FUNCTION,
        Type => CompletionItemKind::CLASS,
        Module => CompletionItemKind::MODULE,
        Import => CompletionItemKind::MODULE,
    }
}

/// Classify a library name without a module lookup: a macro is a function, a
/// `CamelCase` name is (by Julia convention) a type or module, anything else a
/// function. Precise kinds come from member completion, which has the module.
fn heuristic_kind(name: &str, ns: Namespace) -> CompletionItemKind {
    if ns == Namespace::Macro {
        return CompletionItemKind::FUNCTION;
    }
    match name.chars().next() {
        Some(c) if c.is_uppercase() => CompletionItemKind::CLASS,
        _ => CompletionItemKind::FUNCTION,
    }
}

// --- member completion -----------------------------------------------------

/// Every name defined in the library module named by `receiver`, or empty when
/// the receiver does not resolve to a harvested module. `macro_member` (the
/// `Foo.@` case) keeps only macros; otherwise macros are dropped and the rest
/// kept.
fn member_completions<P: PackageSource>(
    packages: &P,
    receiver: &[String],
    macro_member: bool,
) -> Vec<CompletionItem> {
    let Some((head, tail)) = receiver.split_first() else {
        return Vec::new();
    };
    let Some(pkg) = packages.package(head) else {
        return Vec::new();
    };
    let tail: Vec<&str> = tail.iter().map(String::as_str).collect();
    let Some(module) = resolve_submodule(&pkg.root, &tail) else {
        return Vec::new();
    };
    member_items(module, receiver, macro_member)
}

/// The items for a resolved module's defined names.
fn member_items(module: &ModuleIndex, path: &[String], macro_member: bool) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    if macro_member {
        for m in &module.macros {
            items.push(member_item(&m.name, CompletionItemKind::FUNCTION, path));
        }
        return items;
    }
    for f in &module.functions {
        // A qualified extension (`Base.show`) is not a name of this module.
        if f.owner.is_none() {
            items.push(member_item(&f.name, CompletionItemKind::FUNCTION, path));
        }
    }
    for t in &module.types {
        items.push(member_item(&t.name, CompletionItemKind::CLASS, path));
    }
    for c in &module.consts {
        items.push(member_item(&c.name, CompletionItemKind::CONSTANT, path));
    }
    for s in &module.submodules {
        items.push(member_item(&s.name, CompletionItemKind::MODULE, path));
    }
    items
}

fn member_item(name: &str, kind: CompletionItemKind, module_path: &[String]) -> CompletionItem {
    CompletionItem {
        label: name.to_string(),
        kind: Some(kind),
        detail: module_path.last().cloned(),
        data: resolve_data(module_path, name),
        ..Default::default()
    }
}

// --- resolve (lazy docs) ---------------------------------------------------

/// The resolve step, taking a `lookup` from package name to its index so it can
/// be unit-tested without a salsa db.
fn resolve_completion_with(
    lookup: &dyn Fn(&str) -> Option<crate::index::PackageIndex>,
    mut item: CompletionItem,
) -> CompletionItem {
    let Some(data) = item.data.take() else {
        return item;
    };
    let Ok(data) = serde_json::from_value::<ResolveData>(data) else {
        return item;
    };
    let Some((head, tail)) = data.module_path.split_first() else {
        return item;
    };
    let Some(pkg) = lookup(head) else {
        return item;
    };
    let tail: Vec<&str> = tail.iter().map(String::as_str).collect();
    let Some(module) = resolve_submodule(&pkg.root, &tail) else {
        return item;
    };
    enrich(&mut item, module, &data.name);
    item
}

// Signature and type rendering moved to `super::render`, shared with hover.

/// Fill `item`'s signature detail and documentation from the definition of
/// `name` in `module`, searching functions, types, consts, then macros.
fn enrich(item: &mut CompletionItem, module: &ModuleIndex, name: &str) {
    if name.starts_with('@') {
        if let Some(m) = module.macros.iter().find(|m| m.name == name) {
            set_doc(item, m.doc.as_ref().map(|d| d.text.as_str()));
        }
        return;
    }
    if let Some(f) = module.functions.iter().find(|f| f.name == name) {
        item.detail = Some(function_detail(f));
        set_doc(item, f.doc.as_ref().map(|d| d.text.as_str()));
        return;
    }
    if let Some(t) = module.types.iter().find(|t| t.name == name) {
        item.detail = Some(type_detail(t));
        set_doc(item, t.doc.as_ref().map(|d| d.text.as_str()));
        return;
    }
    if let Some(c) = module.consts.iter().find(|c| c.name == name) {
        if let Some(repr) = &c.value_repr {
            item.detail = Some(format!("{name} = {repr}"));
        }
        set_doc(item, c.doc.as_ref().map(|d| d.text.as_str()));
    }
}

fn set_doc(item: &mut CompletionItem, doc: Option<&str>) {
    if let Some(text) = doc {
        item.documentation = Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: text.to_string(),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use crate::index::model::{DefLocation, ExportedName, PackageIndex, Span, Visibility};
    use crate::index::{
        ConstDef, FunctionGroup, MacroDef, Method, Param, TypeDef, TypeExpr, TypeKind,
    };

    fn loc() -> DefLocation {
        DefLocation {
            file: "src/x.jl".into(),
            range: Span { start: 0, end: 0 },
        }
    }

    /// A module with the given name, exports, and defined members.
    fn module(name: &str, exports: &[&str]) -> ModuleIndex {
        ModuleIndex {
            name: name.to_string(),
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
        }
    }

    fn package(root: ModuleIndex) -> Arc<PackageIndex> {
        Arc::new(PackageIndex {
            name: root.name.clone(),
            root,
            diagnostics: Vec::new(),
        })
    }

    fn library(pkgs: Vec<Arc<PackageIndex>>) -> BTreeMap<String, Arc<PackageIndex>> {
        pkgs.into_iter().map(|p| (p.name.clone(), p)).collect()
    }

    fn func(name: &str) -> FunctionGroup {
        FunctionGroup {
            name: name.to_string(),
            owner: None,
            methods: Vec::new(),
            doc: None,
        }
    }

    /// Completions at the position just past `needle` in `src`.
    fn completions_at(
        src: &str,
        needle: &str,
        lib: &BTreeMap<String, Arc<PackageIndex>>,
    ) -> Vec<CompletionItem> {
        let offset = src.find(needle).unwrap() + needle.len();
        let line_index = LineIndex::new(src);
        let position = line_index.byte_to_position(offset, PositionEncoding::Utf16);
        compute_completions(src, position, PositionEncoding::Utf16, lib)
    }

    fn labels(items: &[CompletionItem]) -> Vec<String> {
        items.iter().map(|i| i.label.clone()).collect()
    }

    /// Completions at the position just past `needle`, resolving free names
    /// against `workspace` (the enclosing package's module) too.
    fn completions_ws(
        src: &str,
        needle: &str,
        lib: &BTreeMap<String, Arc<PackageIndex>>,
        workspace: Arc<PackageIndex>,
    ) -> Vec<CompletionItem> {
        let model = SemanticModel::build(&parse(src).cst);
        let offset = TextSize::new((src.find(needle).unwrap() + needle.len()) as u32);
        completions_for(&model, lib, Some(workspace), src, offset)
    }

    #[test]
    fn value_context_offers_workspace_siblings() {
        // `sibling`, a top-level function of the enclosing workspace package,
        // is offered even though it is not defined in this file.
        let lib = library(vec![package(module("Base", &["println"]))]);
        let ws = package(ModuleIndex {
            functions: vec![func("sibling")],
            ..module("MyPkg", &[])
        });
        let src = "function f()\n    \nend";
        let items = completions_ws(src, "    ", &lib, ws);
        let names = labels(&items);
        assert!(names.contains(&"sibling".to_string()), "{names:?}");
        // The sibling ranks after locals and before Base.
        assert!(
            names.iter().position(|n| n == "sibling") < names.iter().position(|n| n == "println")
        );
    }

    #[test]
    fn value_context_lists_locals_before_library_and_includes_keywords() {
        let lib = library(vec![package(module("Base", &["println"]))]);
        let src = "function f(a)\n    b = 1\n    \nend";
        let items = completions_at(src, "b = 1\n    ", &lib);
        let names = labels(&items);
        for expected in ["a", "b", "f", "println", "function", "end"] {
            assert!(names.contains(&expected.to_string()), "missing {expected}");
        }
        // Locals precede the library name.
        assert!(names.iter().position(|n| n == "b") < names.iter().position(|n| n == "println"));
        // A keyword is a KEYWORD item.
        let kw = items.iter().find(|i| i.label == "function").unwrap();
        assert_eq!(kw.kind, Some(CompletionItemKind::KEYWORD));
    }

    #[test]
    fn shadowed_name_appears_once() {
        let lib = library(vec![package(module("Base", &["map"]))]);
        let src = "function f()\n    map = 1\n    \nend";
        let names = labels(&completions_at(src, "map = 1\n    ", &lib));
        assert_eq!(names.iter().filter(|n| *n == "map").count(), 1);
    }

    #[test]
    fn macro_context_offers_only_at_names() {
        let mut base = module("Base", &["@time", "time"]);
        base.macros.push(MacroDef {
            name: "@time".into(),
            params: Vec::new(),
            doc: None,
            loc: loc(),
        });
        let lib = library(vec![package(base)]);
        let src = "@t";
        let names = labels(&completions_at(src, "@t", &lib));
        assert!(names.contains(&"@time".to_string()));
        assert!(!names.contains(&"time".to_string()));
    }

    #[test]
    fn member_context_lists_defined_names_and_submodules() {
        let mut root = module("A", &[]);
        root.functions.push(func("foo"));
        root.types.push(TypeDef {
            name: "Bar".into(),
            kind: TypeKind::Struct { mutable: false },
            type_params: Vec::new(),
            supertype: None,
            fields: Vec::new(),
            doc: None,
            loc: loc(),
        });
        root.consts.push(ConstDef {
            name: "BAUD".into(),
            value_repr: Some("9600".into()),
            doc: None,
            loc: loc(),
        });
        root.submodules.push(module("Inner", &[]));
        let lib = library(vec![package(root)]);
        let items = completions_at("A.", "A.", &lib);
        let names = labels(&items);
        for expected in ["foo", "Bar", "BAUD", "Inner"] {
            assert!(names.contains(&expected.to_string()), "missing {expected}");
        }
        let bar = items.iter().find(|i| i.label == "Bar").unwrap();
        assert_eq!(bar.kind, Some(CompletionItemKind::CLASS));
        let inner = items.iter().find(|i| i.label == "Inner").unwrap();
        assert_eq!(inner.kind, Some(CompletionItemKind::MODULE));
    }

    #[test]
    fn member_context_walks_a_submodule_chain() {
        let mut inner = module("B", &[]);
        inner.functions.push(func("deep"));
        let mut root = module("A", &[]);
        root.submodules.push(inner);
        let lib = library(vec![package(root)]);
        let names = labels(&completions_at("A.B.", "A.B.", &lib));
        assert_eq!(names, vec!["deep".to_string()]);
    }

    #[test]
    fn macro_member_context_offers_only_macros() {
        let mut root = module("A", &[]);
        root.functions.push(func("plain"));
        root.macros.push(MacroDef {
            name: "@mac".into(),
            params: Vec::new(),
            doc: None,
            loc: loc(),
        });
        let lib = library(vec![package(root)]);
        let names = labels(&completions_at("A.@", "A.@", &lib));
        assert_eq!(names, vec!["@mac".to_string()]);
    }

    #[test]
    fn unknown_receiver_yields_no_members() {
        let lib = library(vec![package(module("A", &[]))]);
        assert!(completions_at("Nope.", "Nope.", &lib).is_empty());
    }

    #[test]
    fn resolve_fills_docs_and_signature() {
        let mut root = module("A", &[]);
        let mut group = func("foo");
        group.doc = Some(crate::index::Docstring {
            text: "does a foo".into(),
            loc: loc(),
        });
        group.methods.push(Method {
            params: vec![Param {
                name: Some("x".into()),
                type_annotation: Some(TypeExpr::Name {
                    path: vec!["Int".into()],
                }),
                default: None,
                is_vararg: false,
            }],
            keyword_params: Vec::new(),
            where_clauses: Vec::new(),
            return_type: None,
            has_body: true,
            doc: None,
            loc: loc(),
        });
        root.functions.push(group);
        let pkg = (*package(root)).clone();
        let item = CompletionItem {
            label: "foo".into(),
            data: resolve_data(&["A".into()], "foo"),
            ..Default::default()
        };
        let resolved = resolve_completion_with(&|name| (name == "A").then(|| pkg.clone()), item);
        assert_eq!(resolved.detail.as_deref(), Some("foo(x::Int)"));
        match resolved.documentation {
            Some(Documentation::MarkupContent(m)) => assert_eq!(m.value, "does a foo"),
            other => panic!("expected markdown docs, got {other:?}"),
        }
    }

    #[test]
    fn context_detection() {
        assert_eq!(context_at("foo", 3), Context::Value);
        assert_eq!(context_at("@ti", 3), Context::Macro);
        assert_eq!(
            context_at("Base.", 5),
            Context::Member {
                receiver: vec!["Base".into()],
                macro_member: false,
            }
        );
        assert_eq!(
            context_at("A.B.foo", 7),
            Context::Member {
                receiver: vec!["A".into(), "B".into()],
                macro_member: false,
            }
        );
        assert_eq!(
            context_at("Base.@ti", 8),
            Context::Member {
                receiver: vec!["Base".into()],
                macro_member: true,
            }
        );
    }
}
