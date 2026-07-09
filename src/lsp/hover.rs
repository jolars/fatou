//! Hover (`textDocument/hover`).
//!
//! The symbol under the cursor is classified against the file's semantic model,
//! in the same three shapes name resolution uses:
//!
//! - a **qualified read** (`Base.map`, `Base.@time`) carries its whole module
//!   path, so the member resolves straight to a harvested module;
//! - an **ordinary occurrence** that binds locally shows the binding's kind (and,
//!   for a function/type/macro, its definition line);
//! - a **free read** resolves through the shared masking order
//!   ([`Resolver::resolve`]) to a `using`'d export or a Base/Core symbol.
//!
//! Library symbols render their signature(s) and docstring as markdown. A
//! function shows its whole method group (multiple dispatch), capped so a
//! Base function with dozens of methods stays readable.

use std::panic::AssertUnwindSafe;
use std::path::Path;

use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind, Position, Range};
use rowan::{TextRange, TextSize};

use crate::incremental::Analysis;
use crate::index::{FunctionGroup, ModuleIndex};
use crate::parser::parse;
use crate::resolve::{Namespace, PackageSource, Resolution, Resolver, resolve_submodule};
use crate::semantic::{BindingId, BindingKind, LoadKind, SemanticModel};
use crate::text::{LineIndex, PositionEncoding};

use super::render::{binding_detail, render_method, render_param, type_detail};

/// At most this many method signatures are listed for a function group; the rest
/// are summarized as a trailing count so a Base function stays readable.
const MAX_METHODS: usize = 10;

/// The hover for `text` at `position`, re-parsing it. Pure and unit-testable;
/// `packages` supplies the library (Base/Core and loaded packages).
pub fn compute_hover<P: PackageSource>(
    text: &str,
    position: Position,
    encoding: PositionEncoding,
    packages: &P,
) -> Option<Hover> {
    let model = SemanticModel::build(&parse(text).cst);
    let line_index = LineIndex::new(text);
    let offset = TextSize::new(line_index.position_to_byte(position, encoding) as u32);
    hover_for(&model, packages, text, offset, &line_index, encoding)
}

/// Compute the hover off the snapshot's cached parse when the db's tracked buffer
/// for `path` still matches `text`; otherwise re-parse. A write racing the read
/// trips `salsa::Cancelled`, which also falls back to a fresh parse. Mirrors
/// [`completion_via_db`](super::completion::completion_via_db).
pub(crate) fn hover_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &str,
    position: Position,
    encoding: PositionEncoding,
) -> Option<Hover> {
    let line_index = LineIndex::new(text);
    let offset = TextSize::new(line_index.position_to_byte(position, encoding) as u32);
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            // The tracked input lags the live buffer; the cached model is stale.
            return None;
        }
        let model = snapshot.semantic_model(file);
        // The inner `Option` is the hover result (a cursor on nothing hoverable
        // is a legitimate `None`); the outer distinguishes that from a cache miss.
        Some(hover_for(
            model,
            snapshot,
            text,
            offset,
            &line_index,
            encoding,
        ))
    }));
    match cached {
        Ok(Some(hover)) => hover,
        // Cache miss (`Ok(None)`) or a racing write (`Err`): re-parse from text.
        Ok(None) | Err(_) => compute_hover(text, position, encoding, snapshot),
    }
}

/// Shared entry point for the fresh-parse and cached-model paths: `model` must be
/// the semantic model of exactly `text`.
fn hover_for<P: PackageSource>(
    model: &SemanticModel,
    packages: &P,
    text: &str,
    offset: TextSize,
    line_index: &LineIndex,
    encoding: PositionEncoding,
) -> Option<Hover> {
    let (value, range) = hover_content(model, packages, text, offset)?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: Some(to_range(range, line_index, encoding)),
    })
}

/// The markdown body and highlight range for the symbol at `offset`, or `None`
/// when nothing there resolves to a binding or a harvested symbol.
fn hover_content<P: PackageSource>(
    model: &SemanticModel,
    packages: &P,
    text: &str,
    offset: TextSize,
) -> Option<(String, TextRange)> {
    // A qualified name (`Foo.bar`, `Base.@time`) carries its whole module path.
    if let Some(q) = model
        .qualified_reads()
        .iter()
        .find(|q| q.range.contains_inclusive(offset))
    {
        let (name, module_path) = q.path.split_last()?;
        let head = module_path.first()?;
        let pkg = packages.package(head)?;
        let rest: Vec<&str> = module_path[1..].iter().map(|s| s.as_str()).collect();
        let module = resolve_submodule(&pkg.root, &rest)?;
        return Some((render_library_symbol(module, name)?, q.range));
    }
    // An ordinary identifier occurrence: local when it binds, else a free read.
    if let Some(ident) = model.ident_at(offset) {
        if let Some(bid) = ident.binding {
            return Some((render_local(model, bid, text), ident.range));
        }
        let ns = if ident.is_macro {
            Namespace::Macro
        } else {
            Namespace::Value
        };
        return Some((
            render_free_read(model, packages, text, &ident.name, offset, ns)?,
            ident.range,
        ));
    }
    // A definition site (hovering a name in its own definition) is not an
    // occurrence, so it is found through the binding arena instead.
    if let Some(bid) = model.binding_at(offset) {
        let range = model.binding(bid).def_range;
        return Some((render_local(model, bid, text), range));
    }
    None
}

/// Render a free (non-local, non-qualified) read by resolving it through the
/// shared masking order to a `using`'d export or a Base/Core symbol.
fn render_free_read<P: PackageSource>(
    model: &SemanticModel,
    packages: &P,
    text: &str,
    name: &str,
    offset: TextSize,
    ns: Namespace,
) -> Option<String> {
    match Resolver::new(model, packages).resolve(name, offset, ns) {
        // A binding the occurrence walk missed but resolution finds: still local.
        Resolution::Binding(bid) => Some(render_local(model, bid, text)),
        Resolution::System { module, name } => {
            let pkg = packages.package(&module)?;
            render_library_symbol(&pkg.root, &name)
        }
        Resolution::Using { module, name } => library_from_using(model, packages, &module, &name),
        Resolution::Unresolved => None,
    }
}

/// Find the module a whole-module `using` brings in and render `name` from it.
/// `module` is the clause's display name (its last component): a plain
/// `using LinearAlgebra` names the package directly; a `using A.B` needs the
/// clause walked from its package root.
fn library_from_using<P: PackageSource>(
    model: &SemanticModel,
    packages: &P,
    module: &str,
    name: &str,
) -> Option<String> {
    if let Some(pkg) = packages.package(module)
        && let Some(rendered) = render_library_symbol(&pkg.root, name)
    {
        return Some(rendered);
    }
    for load in model.module_loads() {
        if load.kind != LoadKind::Using || load.items.is_some() {
            continue;
        }
        let comps = &load.path.components;
        if comps.last().map(|c| c.as_str()) != Some(module) {
            continue;
        }
        let Some(first) = comps.first() else { continue };
        let Some(pkg) = packages.package(first.as_str()) else {
            continue;
        };
        let rest: Vec<&str> = comps[1..].iter().map(|c| c.as_str()).collect();
        if let Some(m) = resolve_submodule(&pkg.root, &rest)
            && let Some(rendered) = render_library_symbol(m, name)
        {
            return Some(rendered);
        }
    }
    None
}

/// Look `name` up among `module`'s defined symbols (macros for an `@` name, then
/// functions, types, consts) and render its signature(s) and docstring. Mirrors
/// the search order of completion's `enrich`.
fn render_library_symbol(module: &ModuleIndex, name: &str) -> Option<String> {
    if name.starts_with('@') {
        let m = module.macros.iter().find(|m| m.name == name)?;
        let mut head = m.name.clone();
        if !m.params.is_empty() {
            let ps: Vec<String> = m.params.iter().map(render_param).collect();
            head.push('(');
            head.push_str(&ps.join(", "));
            head.push(')');
        }
        return Some(markdown(&head, m.doc.as_ref().map(|d| d.text.as_str())));
    }
    if let Some(f) = module.functions.iter().find(|f| f.name == name) {
        return Some(markdown(
            &method_group(f),
            f.doc.as_ref().map(|d| d.text.as_str()),
        ));
    }
    if let Some(t) = module.types.iter().find(|t| t.name == name) {
        return Some(markdown(
            &type_detail(t),
            t.doc.as_ref().map(|d| d.text.as_str()),
        ));
    }
    if let Some(c) = module.consts.iter().find(|c| c.name == name) {
        let head = match &c.value_repr {
            Some(repr) => format!("{name} = {repr}"),
            None => name.to_string(),
        };
        return Some(markdown(&head, c.doc.as_ref().map(|d| d.text.as_str())));
    }
    None
}

/// The method group of a function, one `name(signature)` per method up to
/// [`MAX_METHODS`], with any remainder summarized as a trailing comment.
fn method_group(group: &FunctionGroup) -> String {
    if group.methods.is_empty() {
        return group.name.clone();
    }
    let mut lines: Vec<String> = group
        .methods
        .iter()
        .take(MAX_METHODS)
        .map(|m| format!("{}{}", group.name, render_method(m)))
        .collect();
    let extra = group.methods.len().saturating_sub(MAX_METHODS);
    if extra > 0 {
        let plural = if extra == 1 { "" } else { "s" };
        lines.push(format!("# + {extra} more method{plural}"));
    }
    lines.join("\n")
}

/// Render a local binding: its definition line for a function/type/macro (the
/// signature the user wants), otherwise just its name, tagged with its kind.
fn render_local(model: &SemanticModel, bid: BindingId, text: &str) -> String {
    let binding = model.binding(bid);
    let code = match binding.kind {
        BindingKind::Function | BindingKind::Type | BindingKind::Macro => {
            definition_line(text, binding.def_range.start().into())
                .unwrap_or_else(|| binding.name.to_string())
        }
        _ => binding.name.to_string(),
    };
    format!(
        "```julia\n{code}\n```\n\n*{}*",
        binding_detail(binding.kind)
    )
}

/// The trimmed source line containing byte `offset` (the definition's first
/// line), or `None` when that line is blank.
fn definition_line(text: &str, offset: usize) -> Option<String> {
    let offset = offset.min(text.len());
    let start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let end = text[offset..]
        .find('\n')
        .map(|i| offset + i)
        .unwrap_or(text.len());
    let line = text[start..end].trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// A `julia` code block for `code`, with `doc` (already markdown) below a rule.
fn markdown(code: &str, doc: Option<&str>) -> String {
    let mut out = format!("```julia\n{code}\n```");
    if let Some(doc) = doc {
        let doc = doc.trim();
        if !doc.is_empty() {
            out.push_str("\n\n---\n\n");
            out.push_str(doc);
        }
    }
    out
}

fn to_range(range: TextRange, line_index: &LineIndex, encoding: PositionEncoding) -> Range {
    Range {
        start: line_index.byte_to_position(range.start().into(), encoding),
        end: line_index.byte_to_position(range.end().into(), encoding),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use crate::incremental::IncrementalDatabase;
    use crate::index::model::{DefLocation, ExportedName, PackageIndex, Span, Visibility};
    use crate::index::{
        ConstDef, Docstring, FunctionGroup, MacroDef, Method, Param, TypeDef, TypeExpr, TypeKind,
    };

    fn loc() -> DefLocation {
        DefLocation {
            file: "src/x.jl".into(),
            range: Span { start: 0, end: 0 },
        }
    }

    fn doc(text: &str) -> Option<Docstring> {
        Some(Docstring {
            text: text.to_string(),
            loc: loc(),
        })
    }

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

    fn method(params: &[&str]) -> Method {
        Method {
            params: params
                .iter()
                .map(|p| Param {
                    name: Some(p.to_string()),
                    type_annotation: None,
                    default: None,
                    is_vararg: false,
                })
                .collect(),
            keyword_params: Vec::new(),
            where_clauses: Vec::new(),
            return_type: None,
            has_body: true,
            doc: None,
            loc: loc(),
        }
    }

    /// The hover markdown at the position just past `needle` in `src`.
    fn hover_at(
        src: &str,
        needle: &str,
        lib: &BTreeMap<String, Arc<PackageIndex>>,
    ) -> Option<String> {
        let offset = src.find(needle).unwrap() + needle.len();
        let line_index = LineIndex::new(src);
        let position = line_index.byte_to_position(offset, PositionEncoding::Utf16);
        compute_hover(src, position, PositionEncoding::Utf16, lib).map(|h| match h.contents {
            HoverContents::Markup(m) => m.value,
            _ => panic!("expected markup hover"),
        })
    }

    #[test]
    fn local_parameter_shows_kind() {
        let lib = library(vec![]);
        let value = hover_at("function f(abc)\n    abc\nend", "    ab", &lib).unwrap();
        assert!(value.contains("abc"), "{value}");
        assert!(value.contains("*parameter*"), "{value}");
    }

    #[test]
    fn local_function_shows_signature() {
        let lib = library(vec![]);
        // Hover the call site of a function defined in the same file.
        let value = hover_at(
            "function greet(a, b)\n    a + b\nend\ngreet(1, 2)",
            "gre",
            &lib,
        )
        .unwrap();
        assert!(value.contains("function greet(a, b)"), "{value}");
        assert!(value.contains("*function*"), "{value}");
    }

    #[test]
    fn library_function_shows_method_group_and_docstring() {
        let mut base = module("Base", &["map"]);
        base.functions.push(FunctionGroup {
            name: "map".into(),
            owner: None,
            methods: vec![method(&["f", "iter"]), method(&["f", "A"])],
            doc: doc("Transform collection `iter` by applying `f`."),
        });
        let lib = library(vec![package(base)]);
        let value = hover_at("map(sin, xs)", "ma", &lib).unwrap();
        assert!(value.contains("map(f, iter)"), "{value}");
        assert!(value.contains("map(f, A)"), "{value}");
        assert!(value.contains("Transform collection"), "{value}");
    }

    #[test]
    fn method_group_is_capped_with_a_count() {
        let mut base = module("Base", &["f"]);
        base.functions.push(FunctionGroup {
            name: "f".into(),
            owner: None,
            methods: (0..15).map(|_| method(&["x"])).collect(),
            doc: None,
        });
        let lib = library(vec![package(base)]);
        let value = hover_at("f(1)", "f", &lib).unwrap();
        assert_eq!(value.matches("f(x)").count(), MAX_METHODS);
        assert!(value.contains("# + 5 more methods"), "{value}");
    }

    #[test]
    fn library_type_shows_definition() {
        let mut base = module("Base", &["Dict"]);
        base.types.push(TypeDef {
            name: "Dict".into(),
            kind: TypeKind::Struct { mutable: true },
            type_params: Vec::new(),
            supertype: Some(TypeExpr::Name {
                path: vec!["AbstractDict".into()],
            }),
            fields: Vec::new(),
            doc: doc("A hash table."),
            loc: loc(),
        });
        let lib = library(vec![package(base)]);
        let value = hover_at("Dict()", "Di", &lib).unwrap();
        assert!(
            value.contains("mutable struct Dict <: AbstractDict"),
            "{value}"
        );
        assert!(value.contains("A hash table."), "{value}");
    }

    #[test]
    fn library_macro_shows_at_name() {
        let mut base = module("Base", &["@time"]);
        base.macros.push(MacroDef {
            name: "@time".into(),
            params: vec![Param {
                name: Some("expr".into()),
                type_annotation: None,
                default: None,
                is_vararg: false,
            }],
            doc: doc("Time an expression."),
            loc: loc(),
        });
        let lib = library(vec![package(base)]);
        let value = hover_at("@time f()", "@ti", &lib).unwrap();
        assert!(value.contains("@time(expr)"), "{value}");
        assert!(value.contains("Time an expression."), "{value}");
    }

    #[test]
    fn qualified_read_resolves_member() {
        let mut root = module("LinearAlgebra", &[]);
        root.functions.push(FunctionGroup {
            name: "norm".into(),
            owner: None,
            methods: vec![method(&["x"])],
            doc: doc("The norm."),
        });
        let lib = library(vec![package(root)]);
        let value = hover_at("LinearAlgebra.norm(v)", "LinearAlgebra.no", &lib).unwrap();
        assert!(value.contains("norm(x)"), "{value}");
        assert!(value.contains("The norm."), "{value}");
    }

    #[test]
    fn const_shows_value() {
        let mut base = module("Base", &["pi"]);
        base.consts.push(ConstDef {
            name: "pi".into(),
            value_repr: Some("3.14159".into()),
            doc: doc("The constant pi."),
            loc: loc(),
        });
        let lib = library(vec![package(base)]);
        let value = hover_at("pi", "p", &lib).unwrap();
        assert!(value.contains("pi = 3.14159"), "{value}");
    }

    #[test]
    fn unresolved_name_has_no_hover() {
        let lib = library(vec![]);
        assert!(hover_at("unknown_symbol", "unk", &lib).is_none());
    }

    #[test]
    fn hover_via_db_matches_compute_and_falls_back() {
        let path = Path::new("/work/a.jl");
        let mut base = module("Base", &["map"]);
        base.functions.push(FunctionGroup {
            name: "map".into(),
            owner: None,
            methods: vec![method(&["f", "iter"])],
            doc: doc("Map."),
        });
        let lib = library(vec![package(base)]);
        let buffer = "map(sin, xs)\n";
        let position = {
            let li = LineIndex::new(buffer);
            li.byte_to_position(1, PositionEncoding::Utf8)
        };
        let expected = compute_hover(buffer, position, PositionEncoding::Utf8, &lib);
        assert!(expected.is_some());

        let mut db = IncrementalDatabase::default();
        db.set_library_packages(lib.clone());
        db.upsert_file(path, buffer.to_string());

        // Cache hit: tracked text equals the live buffer.
        assert_eq!(
            hover_via_db(
                &db.snapshot(),
                path,
                buffer,
                position,
                PositionEncoding::Utf8
            ),
            expected
        );
        // Stale buffer: the tracked text differs, so it re-parses `other`.
        let other = "xs\n";
        assert_eq!(
            hover_via_db(
                &db.snapshot(),
                path,
                other,
                position,
                PositionEncoding::Utf8
            ),
            compute_hover(other, position, PositionEncoding::Utf8, &db.snapshot())
        );
    }
}
