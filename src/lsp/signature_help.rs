//! Signature help (`textDocument/signatureHelp`).
//!
//! When the cursor sits inside a call's argument list, the callee is resolved
//! the same way hover resolves a name, and its signature(s) are returned with
//! the parameter under the cursor highlighted:
//!
//! - a **library** callee (Base/Core or a `using`'d/qualified package symbol)
//!   yields its whole method group (multiple dispatch), one signature each;
//! - an **intra-file** function shows the single signature read off its own
//!   definition.
//!
//! The active parameter follows the cursor: the count of top-level commas gives
//! the positional index (clamped into a trailing `x...` vararg), and once past
//! the `;` the active parameter is the keyword argument being typed, matched to
//! the method's keyword parameters by name.
//!
//! Deferred, mirroring hover's documented scope: broadcast calls (`f.(...)`),
//! macro-call and `Curly`/type-parameter argument lists, and constructor calls
//! on types. Dispatch narrowing is arity-only (no type-based filtering of
//! applicable methods).

use std::panic::AssertUnwindSafe;
use std::path::Path;

use lsp_types::{
    Documentation, MarkupContent, MarkupKind, ParameterInformation, ParameterLabel, Position,
    SignatureHelp, SignatureInformation,
};
use rowan::{TextSize, TokenAtOffset};

use crate::ast::{AstNode, AstToken, CallExpr, Expr, HasArgList, KeywordArg};
use crate::incremental::Analysis;
use crate::index::{FunctionGroup, Method, ModuleIndex, Param};
use crate::parser::parse;
use crate::resolve::{Namespace, PackageSource, Resolution, Resolver, resolve_submodule};
use crate::semantic::{BindingKind, SemanticModel};
use crate::syntax::{SyntaxKind, SyntaxNode};
use crate::text::{LineIndex, PositionEncoding};

use super::render::signature_label;

/// At most this many method signatures are listed for a function group, matching
/// hover's cap so a Base function with dozens of methods stays readable.
const MAX_METHODS: usize = 10;

/// Signature help for `text` at `position`, re-parsing it. Pure and
/// unit-testable; `packages` supplies the library (Base/Core and loaded
/// packages).
pub fn compute_signature_help<P: PackageSource>(
    text: &str,
    position: Position,
    encoding: PositionEncoding,
    packages: &P,
) -> Option<SignatureHelp> {
    let parsed = parse(text);
    let model = SemanticModel::build(&parsed.cst);
    let offset = TextSize::new(LineIndex::new(text).position_to_byte(position, encoding) as u32);
    signature_help_for(&model, packages, &parsed.cst, offset)
}

/// Compute signature help off the snapshot's cached parse when the db's tracked
/// buffer for `path` still matches `text`; otherwise re-parse. A write racing
/// the read trips `salsa::Cancelled`, which also falls back to a fresh parse.
/// Mirrors [`hover_via_db`](super::hover::hover_via_db).
pub(crate) fn signature_help_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &str,
    position: Position,
    encoding: PositionEncoding,
) -> Option<SignatureHelp> {
    let offset = TextSize::new(LineIndex::new(text).position_to_byte(position, encoding) as u32);
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            // The tracked input lags the live buffer; the cached model is stale.
            return None;
        }
        let model = snapshot.semantic_model(file);
        let root = snapshot.parsed_tree(file);
        // The inner `Option` is the help result (a cursor outside any call is a
        // legitimate `None`); the outer distinguishes that from a cache miss.
        Some(signature_help_for(model, snapshot, &root, offset))
    }));
    match cached {
        Ok(Some(help)) => help,
        // Cache miss (`Ok(None)`) or a racing write (`Err`): re-parse from text.
        Ok(None) | Err(_) => compute_signature_help(text, position, encoding, snapshot),
    }
}

/// Shared entry point for the fresh-parse and cached-model paths: `model` and
/// `root` must both come from exactly the parse of the current buffer.
fn signature_help_for<P: PackageSource>(
    model: &SemanticModel,
    packages: &P,
    root: &SyntaxNode,
    offset: TextSize,
) -> Option<SignatureHelp> {
    let call = enclosing_call(root, offset)?;
    let arg_list = call.arg_list()?;
    let active = active_argument(&arg_list, offset);
    let sig = resolve_callee(model, packages, root, &call)?;
    build_signature_help(&sig, &active)
}

/// The innermost `CALL_EXPR` whose argument list encloses the cursor (strictly
/// after `(`, up to and including the `)`). Walking up from the token at the
/// cursor reaches the innermost call first, so `f(g(|))` selects `g`.
fn enclosing_call(root: &SyntaxNode, offset: TextSize) -> Option<CallExpr> {
    let token = match root.token_at_offset(offset) {
        TokenAtOffset::None => return None,
        TokenAtOffset::Single(t) => t,
        // Between two tokens (e.g. `f(|)`): the left token's ancestry still
        // reaches the enclosing ARG_LIST/CALL_EXPR.
        TokenAtOffset::Between(left, _) => left,
    };
    token.parent_ancestors().find_map(|node| {
        let call = CallExpr::cast(node)?;
        let range = call.arg_list()?.syntax().text_range();
        (range.start() < offset && offset <= range.end()).then_some(call)
    })
}

/// Where the cursor sits within an argument list.
enum Active {
    /// Among the positional arguments, at this zero-based index.
    Positional(usize),
    /// Among the keyword arguments (past the `;`), naming the keyword under the
    /// cursor when one is being typed.
    Keyword(Option<String>),
}

/// Classify the cursor within `arg_list`: count top-level commas before the `;`
/// for the positional index, and detect the keyword region (a `PARAMETERS`
/// section or a `KEYWORD_ARG` under the cursor).
fn active_argument(arg_list: &crate::ast::ArgList, offset: TextSize) -> Active {
    let mut positional = 0usize;
    let mut in_keywords = false;
    for el in arg_list.syntax().children_with_tokens() {
        if el.text_range().start() >= offset {
            break;
        }
        match el.kind() {
            SyntaxKind::COMMA => positional += 1,
            // The `; keyword...` section is a nested PARAMETERS node; entering it
            // (or reaching its bare `;`) switches to the keyword region.
            SyntaxKind::PARAMETERS | SyntaxKind::SEMICOLON => in_keywords = true,
            _ => {}
        }
    }
    // A keyword name directly under the cursor, for name matching.
    let active_keyword = arg_list
        .syntax()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::KEYWORD_ARG)
        .find(|n| n.text_range().contains_inclusive(offset))
        .and_then(KeywordArg::cast)
        .and_then(|k| k.name())
        .and_then(|nm| nm.ident())
        .map(|id| id.text().to_string());
    if in_keywords || active_keyword.is_some() {
        Active::Keyword(active_keyword)
    } else {
        Active::Positional(positional)
    }
}

/// A callee resolved to a renderable set of signatures.
struct CalleeSig {
    name: String,
    methods: Vec<Method>,
    doc: Option<String>,
}

/// Resolve the call's callee to its signature(s): a qualified `Mod.f`, a bare
/// name (through the shared masking order), or an intra-file function.
fn resolve_callee<P: PackageSource>(
    model: &SemanticModel,
    packages: &P,
    root: &SyntaxNode,
    call: &CallExpr,
) -> Option<CalleeSig> {
    let callee = call.callee()?;
    let callee_range = callee.syntax().text_range();

    // A qualified callee (`Base.map`, `LinearAlgebra.norm`) is recorded whole as
    // a qualified read; resolve its member straight to the harvested module.
    if let Some(q) = model
        .qualified_reads()
        .iter()
        .find(|q| q.range == callee_range)
    {
        let (name, module_path) = q.path.split_last()?;
        let head = module_path.first()?;
        let pkg = packages.package(head)?;
        let rest: Vec<&str> = module_path[1..].iter().map(|s| s.as_str()).collect();
        let module = resolve_submodule(&pkg.root, &rest)?;
        return library_callee(module, name);
    }

    // Otherwise a bare name callee: resolve it like a free read.
    let Expr::Name(name_node) = &callee else {
        return None;
    };
    let name = name_node.ident()?.text().to_string();
    let name_offset = name_node.syntax().text_range().start();
    match Resolver::new(model, packages).resolve(&name, name_offset, Namespace::Value) {
        Resolution::Binding(bid) => {
            let binding = model.binding(bid);
            if binding.kind != BindingKind::Function {
                return None;
            }
            local_callee(root, binding.def_range.start(), &binding.name)
        }
        Resolution::System { module, name } => {
            let pkg = packages.package(&module)?;
            library_callee(&pkg.root, &name)
        }
        Resolution::Using { module, name } => {
            let pkg = packages.package(&module)?;
            library_callee(&pkg.root, &name)
        }
        // This resolver carries no workspace context, so a same-module sibling
        // never reaches here; signature help for workspace functions is a later
        // Phase 5 item.
        Resolution::Workspace { .. } => None,
        Resolution::Unresolved => None,
    }
}

/// The method group of `name` in `module`, as a [`CalleeSig`].
fn library_callee(module: &ModuleIndex, name: &str) -> Option<CalleeSig> {
    let group: &FunctionGroup = module.functions.iter().find(|f| f.name == name)?;
    Some(CalleeSig {
        name: group.name.clone(),
        methods: group.methods.clone(),
        doc: group.doc.as_ref().map(|d| d.text.clone()),
    })
}

/// The single signature of an intra-file function, read off the parameter list
/// at its definition. `def_start` is the definition name's offset; the enclosing
/// `CALL_EXPR` is the signature `name(params; keywords)`.
fn local_callee(root: &SyntaxNode, def_start: TextSize, name: &str) -> Option<CalleeSig> {
    let sig_call = root
        .token_at_offset(def_start)
        .right_biased()?
        .parent_ancestors()
        .find_map(CallExpr::cast)?;
    let arg_list = sig_call.arg_list()?;
    let mut params: Vec<Param> = Vec::new();
    let mut keyword_params: Vec<Param> = Vec::new();
    for el in arg_list.syntax().children() {
        match el.kind() {
            SyntaxKind::ARG | SyntaxKind::KEYWORD_ARG => params.push(param_from_source(&el)),
            // The `; keyword...` section holds the keyword parameters.
            SyntaxKind::PARAMETERS => {
                for kw in el.children() {
                    if matches!(kw.kind(), SyntaxKind::ARG | SyntaxKind::KEYWORD_ARG) {
                        keyword_params.push(param_from_source(&kw));
                    }
                }
            }
            _ => {}
        }
    }
    let method = Method {
        params,
        keyword_params,
        where_clauses: Vec::new(),
        return_type: None,
        has_body: true,
        doc: None,
        loc: crate::index::model::DefLocation {
            file: Default::default(),
            range: crate::index::model::Span { start: 0, end: 0 },
        },
    };
    Some(CalleeSig {
        name: name.to_string(),
        methods: vec![method],
        doc: None,
    })
}

/// A synthetic [`Param`] carrying an argument's source text verbatim as its
/// name, so [`render_param`](super::render::render_param) reproduces it exactly
/// (`x`, `y::Int`, `z = 1`). Used for intra-file signatures, whose parameter
/// types are not lowered to a [`crate::index::TypeExpr`].
fn param_from_source(node: &SyntaxNode) -> Param {
    Param {
        name: Some(
            node.text()
                .to_string()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        ),
        type_annotation: None,
        default: None,
        is_vararg: false,
    }
}

/// Assemble the `SignatureHelp` from the resolved signatures and the cursor's
/// active argument, highlighting the active parameter per method.
fn build_signature_help(sig: &CalleeSig, active: &Active) -> Option<SignatureHelp> {
    if sig.methods.is_empty() {
        return None;
    }
    let doc = sig.doc.as_ref().map(|text| {
        Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: text.clone(),
        })
    });
    let signatures: Vec<SignatureInformation> = sig
        .methods
        .iter()
        .take(MAX_METHODS)
        .map(|method| {
            let (label, spans) = signature_label(&sig.name, method);
            let parameters = spans
                .iter()
                .map(|&(start, end)| ParameterInformation {
                    label: ParameterLabel::LabelOffsets([start, end]),
                    documentation: None,
                })
                .collect();
            SignatureInformation {
                label,
                documentation: doc.clone(),
                parameters: Some(parameters),
                active_parameter: active_parameter(method, active),
            }
        })
        .collect();

    // The active signature is the first whose arity fits the current call.
    let active_signature = sig
        .methods
        .iter()
        .take(MAX_METHODS)
        .position(|m| fits(m, active))
        .unwrap_or(0) as u32;
    let active_parameter = signatures
        .get(active_signature as usize)
        .and_then(|s| s.active_parameter);

    Some(SignatureHelp {
        signatures,
        active_signature: Some(active_signature),
        active_parameter,
    })
}

/// The index of the parameter the cursor is on within `method`, or `None` when
/// the method has no parameter in that position.
fn active_parameter(method: &Method, active: &Active) -> Option<u32> {
    match active {
        Active::Positional(i) => {
            if method.params.is_empty() {
                return None;
            }
            // A trailing `x...` swallows every further positional argument.
            let idx = match method.params.iter().position(|p| p.is_vararg) {
                Some(vararg) => (*i).min(vararg),
                None => (*i).min(method.params.len() - 1),
            };
            Some(idx as u32)
        }
        Active::Keyword(name) => {
            let base = method.params.len();
            if let Some(name) = name
                && let Some(k) = method
                    .keyword_params
                    .iter()
                    .position(|p| p.name.as_deref() == Some(name.as_str()))
            {
                return Some((base + k) as u32);
            }
            // A keyword being typed (or an unrecognized one) points at the first
            // keyword parameter, if the method has any.
            (!method.keyword_params.is_empty()).then_some(base as u32)
        }
    }
}

/// Whether `method`'s arity can accommodate the cursor's active argument.
fn fits(method: &Method, active: &Active) -> bool {
    match active {
        Active::Positional(i) => {
            method.params.len() > *i || method.params.iter().any(|p| p.is_vararg)
        }
        Active::Keyword(_) => !method.keyword_params.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use crate::incremental::IncrementalDatabase;
    use crate::index::model::{DefLocation, ExportedName, PackageIndex, Span, Visibility};
    use crate::index::{Docstring, FunctionGroup, Method, Param};

    fn loc() -> DefLocation {
        DefLocation {
            file: "src/x.jl".into(),
            range: Span { start: 0, end: 0 },
        }
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

    fn param(name: &str) -> Param {
        Param {
            name: Some(name.to_string()),
            type_annotation: None,
            default: None,
            is_vararg: false,
        }
    }

    fn vararg(name: &str) -> Param {
        Param {
            is_vararg: true,
            ..param(name)
        }
    }

    fn method(params: &[Param], keyword_params: &[Param]) -> Method {
        Method {
            params: params.to_vec(),
            keyword_params: keyword_params.to_vec(),
            where_clauses: Vec::new(),
            return_type: None,
            has_body: true,
            doc: None,
            loc: loc(),
        }
    }

    fn group(name: &str, methods: Vec<Method>, doc: Option<&str>) -> FunctionGroup {
        FunctionGroup {
            name: name.into(),
            owner: None,
            methods,
            doc: doc.map(|text| Docstring {
                text: text.to_string(),
                loc: loc(),
            }),
        }
    }

    /// Signature help at the position marked by `|` in `src` (the marker is
    /// stripped before parsing).
    fn help_at(marked: &str, lib: &BTreeMap<String, Arc<PackageIndex>>) -> Option<SignatureHelp> {
        let offset = marked.find('|').expect("a cursor marker");
        let src = marked.replacen('|', "", 1);
        let line_index = LineIndex::new(&src);
        let position = line_index.byte_to_position(offset, PositionEncoding::Utf16);
        compute_signature_help(&src, position, PositionEncoding::Utf16, lib)
    }

    fn labels(help: &SignatureHelp) -> Vec<&str> {
        help.signatures.iter().map(|s| s.label.as_str()).collect()
    }

    #[test]
    fn library_method_group_and_active_parameter() {
        let mut base = module("Base", &["map"]);
        base.functions.push(group(
            "map",
            vec![
                method(&[param("f"), param("iter")], &[]),
                method(&[param("f"), param("A")], &[]),
            ],
            Some("Transform `iter` by `f`."),
        ));
        let lib = library(vec![package(base)]);
        let help = help_at("map(sin, |xs)", &lib).unwrap();
        assert_eq!(labels(&help), ["map(f, iter)", "map(f, A)"]);
        // Cursor is in the second positional argument.
        assert_eq!(help.active_parameter, Some(1));
        assert_eq!(help.signatures[0].active_parameter, Some(1));
        // The docstring rides along as signature documentation.
        assert!(help.signatures[0].documentation.is_some());
    }

    #[test]
    fn active_parameter_advances_past_commas() {
        let mut base = module("Base", &["f"]);
        base.functions.push(group(
            "f",
            vec![method(&[param("a"), param("b")], &[])],
            None,
        ));
        let lib = library(vec![package(base)]);
        assert_eq!(help_at("f(1|)", &lib).unwrap().active_parameter, Some(0));
        assert_eq!(help_at("f(1, |2)", &lib).unwrap().active_parameter, Some(1));
    }

    #[test]
    fn cursor_right_after_open_paren_is_first_parameter() {
        let mut base = module("Base", &["f"]);
        base.functions.push(group(
            "f",
            vec![method(&[param("a"), param("b")], &[])],
            None,
        ));
        let lib = library(vec![package(base)]);
        assert_eq!(help_at("f(|)", &lib).unwrap().active_parameter, Some(0));
    }

    #[test]
    fn vararg_swallows_further_arguments() {
        let mut base = module("Base", &["g"]);
        base.functions.push(group(
            "g",
            vec![method(&[param("a"), vararg("rest")], &[])],
            None,
        ));
        let lib = library(vec![package(base)]);
        // Third positional still highlights the vararg (index 1).
        assert_eq!(
            help_at("g(1, 2, |3)", &lib).unwrap().active_parameter,
            Some(1)
        );
    }

    #[test]
    fn keyword_argument_highlights_matching_parameter() {
        let mut base = module("Base", &["h"]);
        base.functions.push(group(
            "h",
            vec![method(&[param("a")], &[param("by"), param("rev")])],
            None,
        ));
        let lib = library(vec![package(base)]);
        let help = help_at("h(x; rev|=true)", &lib).unwrap();
        // `a` is index 0, `by` index 1, `rev` index 2.
        assert_eq!(help.active_parameter, Some(2));
        assert_eq!(labels(&help), ["h(a; by, rev)"]);
    }

    #[test]
    fn qualified_callee_resolves_member() {
        let mut root = module("LinearAlgebra", &[]);
        root.functions.push(group(
            "norm",
            vec![method(&[param("x"), param("p")], &[])],
            None,
        ));
        let lib = library(vec![package(root)]);
        let help = help_at("LinearAlgebra.norm(v, |2)", &lib).unwrap();
        assert_eq!(labels(&help), ["norm(x, p)"]);
        assert_eq!(help.active_parameter, Some(1));
    }

    #[test]
    fn intra_file_function_shows_its_signature() {
        let lib = library(vec![]);
        let help = help_at(
            "function greet(a, b::Int; c=1)\n    a\nend\ngreet(1, |2)",
            &lib,
        )
        .unwrap();
        assert_eq!(labels(&help), ["greet(a, b::Int; c=1)"]);
        assert_eq!(help.active_parameter, Some(1));
    }

    #[test]
    fn nested_call_selects_inner_callee() {
        let mut base = module("Base", &["f", "g"]);
        base.functions
            .push(group("f", vec![method(&[param("x")], &[])], None));
        base.functions
            .push(group("g", vec![method(&[param("y")], &[])], None));
        let lib = library(vec![package(base)]);
        let help = help_at("f(g(|))", &lib).unwrap();
        assert_eq!(labels(&help), ["g(y)"]);
    }

    #[test]
    fn outside_a_call_has_no_help() {
        let mut base = module("Base", &["f"]);
        base.functions
            .push(group("f", vec![method(&[param("a")], &[])], None));
        let lib = library(vec![package(base)]);
        assert!(help_at("f(1) |+ 2", &lib).is_none());
    }

    #[test]
    fn unresolved_callee_has_no_help() {
        let lib = library(vec![]);
        assert!(help_at("mystery(|1)", &lib).is_none());
    }

    #[test]
    fn active_signature_prefers_a_fitting_arity() {
        let mut base = module("Base", &["f"]);
        base.functions.push(group(
            "f",
            vec![
                method(&[param("a")], &[]),
                method(&[param("a"), param("b")], &[]),
            ],
            None,
        ));
        let lib = library(vec![package(base)]);
        // Two positional arguments: only the second method fits.
        let help = help_at("f(1, |2)", &lib).unwrap();
        assert_eq!(help.active_signature, Some(1));
        assert_eq!(help.active_parameter, Some(1));
    }

    #[test]
    fn via_db_matches_compute_and_falls_back() {
        let path = Path::new("/work/a.jl");
        let mut base = module("Base", &["map"]);
        base.functions.push(group(
            "map",
            vec![method(&[param("f"), param("iter")], &[])],
            None,
        ));
        let lib = library(vec![package(base)]);
        let buffer = "map(sin, xs)\n";
        let position = LineIndex::new(buffer).byte_to_position(9, PositionEncoding::Utf8);
        let expected = compute_signature_help(buffer, position, PositionEncoding::Utf8, &lib);
        assert!(expected.is_some());

        let mut db = IncrementalDatabase::default();
        db.set_library_packages(lib.clone());
        db.upsert_file(path, buffer.to_string());

        // Cache hit: the tracked text equals the live buffer.
        assert_eq!(
            signature_help_via_db(
                &db.snapshot(),
                path,
                buffer,
                position,
                PositionEncoding::Utf8
            ),
            expected
        );
        // Stale buffer: the tracked text differs, so it re-parses `other`.
        let other = "map(sin, xs, extra)\n";
        assert_eq!(
            signature_help_via_db(
                &db.snapshot(),
                path,
                other,
                position,
                PositionEncoding::Utf8
            ),
            compute_signature_help(other, position, PositionEncoding::Utf8, &db.snapshot())
        );
    }
}
