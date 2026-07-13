//! Type hierarchy (`textDocument/prepareTypeHierarchy`,
//! `typeHierarchy/supertypes`, `typeHierarchy/subtypes`), built from the
//! declared type tree — Julia's nominal `<:` declarations answer both
//! directions exactly, no inference required.
//!
//! Prepare classifies the symbol at the cursor exactly as call hierarchy does —
//! a workspace top-level type through [`cross_file::workspace_symbol_at`], or
//! an intra-file [`BindingKind::Type`] binding — and returns the one item for
//! the declaration. Supertypes re-derives the declaration from the item,
//! extracts the declared supertype's base name (`AbstractArray` from
//! `<: AbstractArray{T,1}`), and resolves it through the shared masking order:
//! intra-file bindings and workspace siblings resolve off tracked text,
//! Base/depot targets materialize their harvested
//! [`DefLocation`](crate::index::model::DefLocation) from disk like
//! go-to-definition does. Julia is single-inheritance, so the answer is one
//! item — or none for the implicit `Any` (no synthesized root; `Any` is a Core
//! built-in without a reliable harvested declaration). Subtypes rides the
//! reverse-occurrence index: every non-definition site of the type is kept iff
//! it is syntactically a declared-supertype position
//! ([`supertype_site_decl`], re-derived from the cached CST — the index does
//! not record supertype-ness), and each such site's enclosing declaration is
//! the subtype.
//!
//! Supertypes and subtypes receive a [`TypeHierarchyItem`], not a live
//! document position: the target is re-derived from `item.uri` plus
//! `item.selection_range` against the snapshot's *tracked* text, which works
//! for closed member files (members are disk-seeded). On position skew or a
//! racing write (`salsa::Cancelled`) they return `None` rather than answering
//! from a stale tree — clients re-prepare after an edit.
//!
//! Deferred: qualified supertypes (`<: Base.Number` — matching
//! `workspace_occurrences`' deferral of qualified reads), imported supertypes
//! (`import Base: Number; struct T <: Number` — imports are not chased,
//! matching hover and go-to-definition), and subtypes of library types (the
//! reverse index only covers workspace members).

use std::panic::AssertUnwindSafe;
use std::path::Path;

use lsp_types::{Position, Range, SymbolKind, TypeHierarchyItem, Uri};
use rowan::{TextRange, TextSize};

use crate::incremental::{Analysis, SourceFile};
use crate::index::model::Span;
use crate::parser::parse;
use crate::resolve::{Namespace, OccurrenceKey, PackageSource, Resolution, Resolver};
use crate::semantic::{BindingKind, SemanticModel};
use crate::syntax::{SyntaxKind, SyntaxNode};
use crate::text::{LineIndex, PositionEncoding};

use super::cross_file;
use super::definition::{library_def_site, using_def_site};
use super::references::binding_at_cursor;
use super::symbols::{head_name, signature_detail, signature_expr, token_at};
use super::uri::{from_path, to_path};

/// A type declaration extracted from the CST: a `struct`/`mutable struct`,
/// `abstract type`, or `primitive type` definition.
struct TypeDecl {
    /// The declared name (`Foo` for `Foo{T} <: Bar`).
    name: String,
    /// The rest of the signature, as document symbols render it
    /// (`{T} <: Bar{T}`).
    detail: Option<String>,
    /// STRUCT for struct/primitive, INTERFACE for abstract — the
    /// document-symbols convention.
    kind: SymbolKind,
    /// The whole definition node.
    range: TextRange,
    /// The name within it.
    selection: TextRange,
}

/// The type declaration `node` introduces, when it is one. Mirrors the shapes
/// document symbols recognize (`def_symbol` in [`symbols`](super::symbols)):
/// a `STRUCT_DEF`/`ABSTRACT_DEF`/`PRIMITIVE_DEF` whose signature names a type,
/// with any `<: Super` clause peeled off the head. A primitive's bit count
/// sits outside the `SIGNATURE`, so it never leaks into `detail`.
fn type_decl_def(node: &SyntaxNode, text: &str) -> Option<TypeDecl> {
    if !matches!(
        node.kind(),
        SyntaxKind::STRUCT_DEF | SyntaxKind::ABSTRACT_DEF | SyntaxKind::PRIMITIVE_DEF
    ) {
        return None;
    }
    let sig_expr = signature_expr(node)?;
    let name_part = match subtype_clause(&sig_expr) {
        Some((name_part, _)) => name_part,
        None => sig_expr.clone(),
    };
    let (name, selection) = head_name(&name_part)?;
    let detail = signature_detail(&sig_expr, selection, text);
    let kind = if node.kind() == SyntaxKind::ABSTRACT_DEF {
        SymbolKind::INTERFACE
    } else {
        SymbolKind::STRUCT
    };
    Some(TypeDecl {
        name,
        detail,
        kind,
        range: node.text_range(),
        selection,
    })
}

/// The `(name part, supertype expr)` of a `Name <: Super` signature head, or
/// `None` when the expr declares no supertype. A single `<:` parses as a
/// `BINARY_EXPR`; a malformed `A <: B <: C` chain folds into a
/// `COMPARISON_EXPR` — both are handled, mirroring harvest's `header_parts`.
/// The `SUBTYPE` token must be a *direct* child, so `Foo{T <: Real}` (whose
/// `<:` nests under the `ARG_LIST`) does not match.
fn subtype_clause(expr: &SyntaxNode) -> Option<(SyntaxNode, SyntaxNode)> {
    if !matches!(
        expr.kind(),
        SyntaxKind::BINARY_EXPR | SyntaxKind::COMPARISON_EXPR
    ) {
        return None;
    }
    let has_subtype = expr
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::SUBTYPE);
    if !has_subtype {
        return None;
    }
    let mut children = expr.children();
    Some((children.next()?, children.next()?))
}

/// The declared supertype expression of a type definition node, if any.
fn declared_supertype(def: &SyntaxNode) -> Option<SyntaxNode> {
    subtype_clause(&signature_expr(def)?).map(|(_, sup)| sup)
}

/// The type declaration whose *name* spans `offset`, with its node: what
/// prepare returns and what a returned item's `selection_range` re-derives.
fn type_decl_at(root: &SyntaxNode, offset: TextSize, text: &str) -> Option<(SyntaxNode, TypeDecl)> {
    let token = token_at(root, offset)?;
    token.parent_ancestors().find_map(|node| {
        let decl = type_decl_def(&node, text)?;
        decl.selection
            .contains_inclusive(offset)
            .then_some((node, decl))
    })
}

/// The plain base name of a declared supertype expression: `Animal` from
/// `Animal`, `AbstractArray` from `AbstractArray{T,1}`. `None` for a
/// qualified path (`Base.Number` — deferred; resolving the dotted text as a
/// plain name would always miss) or anything else (`Union{...}`,
/// interpolation). Deliberately not routed through `head_name`
/// unconditionally: its dot arm returns the whole dotted text.
fn supertype_base(expr: &SyntaxNode) -> Option<(String, TextRange)> {
    match expr.kind() {
        SyntaxKind::NAME | SyntaxKind::NONSTANDARD_IDENTIFIER => head_name(expr),
        SyntaxKind::CURLY_EXPR => {
            let base = expr.first_child()?;
            matches!(
                base.kind(),
                SyntaxKind::NAME | SyntaxKind::NONSTANDARD_IDENTIFIER
            )
            .then(|| head_name(&base))
            .flatten()
        }
        _ => None,
    }
}

/// When the identifier occurrence spanning `range` is syntactically a
/// *declared-supertype* position — the RHS of the `<:` in a type definition's
/// signature, possibly as the base of a parametric application `Super{T}` —
/// the enclosing declaration's [`TypeDecl`]. The semantic model and the
/// reverse index do not record supertype-ness, so it is re-derived from the
/// (cached) CST, like call hierarchy's `is_call_site`. Unlike a call site, a
/// supertype site sits inside exactly one declaration, so validation and
/// container extraction fuse into one step.
fn supertype_site_decl(root: &SyntaxNode, range: TextRange, text: &str) -> Option<TypeDecl> {
    let token = token_at(root, range.start())?;
    // The occurrence covers a NAME (or `var"..."`) node exactly.
    let name = token.parent_ancestors().find(|n| n.text_range() == range)?;
    // `Super{T}`: step up to the CURLY_EXPR when the name is its *base*
    // (first child). A name inside the ARG_LIST is a type argument, not the
    // supertype (`struct S <: Tree{Animal}` does not make S an Animal).
    let mut node = name;
    if let Some(parent) = node.parent()
        && parent.kind() == SyntaxKind::CURLY_EXPR
        && parent.first_child().as_ref() == Some(&node)
    {
        node = parent;
    }
    // The node must be the supertype operand of the clause, and the clause a
    // type definition's signature — which rejects curly type-param bounds
    // (parent is an ARG_LIST), `where` bounds (the def is a function), and
    // runtime `A <: B` tests (parent is not a SIGNATURE).
    let clause = node.parent()?;
    let (_, sup) = subtype_clause(&clause)?;
    if sup != node {
        return None;
    }
    let sig = clause.parent()?;
    if sig.kind() != SyntaxKind::SIGNATURE {
        return None;
    }
    type_decl_def(&sig.parent()?, text)
}

fn to_range(range: TextRange, line_index: &LineIndex, encoding: PositionEncoding) -> Range {
    Range {
        start: line_index.byte_to_position(range.start().into(), encoding),
        end: line_index.byte_to_position(range.end().into(), encoding),
    }
}

/// The item for a type declaration.
fn item_for(
    uri: &Uri,
    decl: &TypeDecl,
    line_index: &LineIndex,
    encoding: PositionEncoding,
) -> TypeHierarchyItem {
    TypeHierarchyItem {
        name: decl.name.clone(),
        kind: decl.kind,
        tags: None,
        detail: decl.detail.clone(),
        uri: uri.clone(),
        range: to_range(decl.range, line_index, encoding),
        selection_range: to_range(decl.selection, line_index, encoding),
        data: None,
    }
}

// ---------------------------------------------------------------------------
// Prepare
// ---------------------------------------------------------------------------

/// The type-hierarchy item for the type at `position` in `text`, re-parsing
/// it. Pure and unit-testable; single-file, so a free read (a library or
/// workspace symbol) yields `None` here — the via-db path resolves those
/// through the reverse-occurrence index.
pub fn compute_prepare_type_hierarchy(
    uri: &Uri,
    text: &str,
    position: Position,
    encoding: PositionEncoding,
) -> Option<Vec<TypeHierarchyItem>> {
    let root = parse(text).cst;
    let model = SemanticModel::build(&root);
    let line_index = LineIndex::new(text);
    let offset = TextSize::new(line_index.position_to_byte(position, encoding) as u32);
    prepare_for(&root, &model, uri, text, &line_index, offset, encoding)
}

/// The intra-file classification shared by both prepare paths: the cursor
/// names a [`BindingKind::Type`] binding (its declaration, a supertype use, a
/// field annotation, or a constructor call), whose declaration supplies the
/// item.
fn prepare_for(
    root: &SyntaxNode,
    model: &SemanticModel,
    uri: &Uri,
    text: &str,
    line_index: &LineIndex,
    offset: TextSize,
    encoding: PositionEncoding,
) -> Option<Vec<TypeHierarchyItem>> {
    let binding = binding_at_cursor(model, offset)?;
    let binding = model.binding(binding);
    if binding.kind != BindingKind::Type {
        return None;
    }
    let (_, decl) = type_decl_at(root, binding.def_range.start(), text)?;
    Some(vec![item_for(uri, &decl, line_index, encoding)])
}

/// Prepare off the snapshot's cached parse when the db's tracked buffer for
/// `path` still matches `text`; otherwise re-parse. A workspace top-level
/// type resolves through the reverse-occurrence index to its defining file;
/// anything else stays intra-file. Mirrors
/// [`prepare_call_hierarchy_via_db`](super::call_hierarchy::prepare_call_hierarchy_via_db).
pub(crate) fn prepare_type_hierarchy_via_db(
    snapshot: &Analysis,
    uri: &Uri,
    path: &Path,
    text: &str,
    position: Position,
    encoding: PositionEncoding,
) -> Option<Vec<TypeHierarchyItem>> {
    let line_index = LineIndex::new(text);
    let offset = TextSize::new(line_index.position_to_byte(position, encoding) as u32);
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            return None;
        }
        let model = snapshot.semantic_model(file);
        // A function or const also keys the index; `workspace_type_item`'s
        // declaration gate rejects those, falling through to the intra-file
        // path whose Type gate rejects them too.
        if let Some(symbol) = cross_file::workspace_symbol_at(snapshot, path, model, offset)
            && symbol.namespace == Namespace::Value
            && let Some(item) = workspace_type_item(snapshot, &symbol, encoding)
        {
            return Some(Some(vec![item]));
        }
        let root = snapshot.parsed_tree(file);
        Some(prepare_for(
            &root,
            model,
            uri,
            text,
            &line_index,
            offset,
            encoding,
        ))
    }));
    match cached {
        Ok(Some(items)) => items,
        Ok(None) | Err(_) => compute_prepare_type_hierarchy(uri, text, position, encoding),
    }
}

/// The item for a workspace symbol's type declaration (first by file, then
/// position — the "first method" rule go-to-definition also uses), built
/// through the defining file's tracked text. `None` when no definition rec is
/// a type declaration — a function or const also keys the index, and outer
/// constructors share the struct's name without being its declaration.
fn workspace_type_item(
    snapshot: &Analysis,
    symbol: &OccurrenceKey,
    encoding: PositionEncoding,
) -> Option<TypeHierarchyItem> {
    let index = snapshot.workspace_reference_index();
    let mut defs: Vec<(SourceFile, TextRange)> = index
        .0
        .get(symbol)?
        .iter()
        .filter(|(_, rec)| rec.is_def)
        .map(|&(file, rec)| (file, rec.range))
        .collect();
    defs.sort_by_key(|&(file, range)| (snapshot.file_path_of(file), range.start()));
    for (file, range) in defs {
        let Some(path) = snapshot.file_path_of(file) else {
            continue;
        };
        let Some(uri) = from_path(&path) else {
            continue;
        };
        let text = snapshot.file_text_of(file);
        let root = snapshot.parsed_tree(file);
        if let Some((_, decl)) = type_decl_at(&root, range.start(), text) {
            let line_index = LineIndex::new(text);
            return Some(item_for(&uri, &decl, &line_index, encoding));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Supertypes
// ---------------------------------------------------------------------------

/// The declared supertype of `item` — zero or one items; Julia is
/// single-inheritance. Re-derived from `item.uri` plus `item.selection_range`
/// against the snapshot's tracked text. `Some(vec![])` for the implicit `Any`
/// (a hierarchy root; no synthesized item) and for deferred shapes (a
/// qualified or imported supertype); `None` when the item cannot be
/// re-derived (position skew, an untracked file) or a write races the read.
/// The supertype name resolves through the shared masking order: an
/// intra-file binding or workspace sibling off tracked text, a Base/depot
/// type from its harvested location on disk.
pub(crate) fn supertypes_via_db(
    snapshot: &Analysis,
    item: &TypeHierarchyItem,
    encoding: PositionEncoding,
) -> Option<Vec<TypeHierarchyItem>> {
    let path = to_path(&item.uri)?;
    let supers = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(&path)?;
        let text = snapshot.file_text(file);
        let line_index = LineIndex::new(text);
        let offset =
            TextSize::new(line_index.position_to_byte(item.selection_range.start, encoding) as u32);
        let root = snapshot.parsed_tree(file);
        let (def, _) = type_decl_at(&root, offset, text)?;
        let Some(sup) = declared_supertype(&def) else {
            // No declared supertype: the implicit `Any`, a hierarchy root.
            return Some(Vec::new());
        };
        let Some((name, name_range)) = supertype_base(&sup) else {
            // Qualified (`Base.Number`) or exotic — deferred.
            return Some(Vec::new());
        };

        let model = snapshot.semantic_model(file);
        let workspace = snapshot.workspace_member(&path);
        let resolver = Resolver::new(model, snapshot).with_workspace(workspace.clone());
        let target = match resolver.resolve(&name, name_range.start(), Namespace::Value) {
            Resolution::Binding(bid) => {
                let binding = model.binding(bid);
                // Imports are not chased into the library (matching hover and
                // go-to-definition).
                if binding.kind != BindingKind::Type {
                    None
                } else {
                    type_decl_at(&root, binding.def_range.start(), text)
                        .map(|(_, decl)| item_for(&item.uri, &decl, &line_index, encoding))
                }
            }
            Resolution::Workspace { module, name } => workspace.as_ref().and_then(|(pkg, _)| {
                workspace_type_item(
                    snapshot,
                    &OccurrenceKey {
                        package: smol_str::SmolStr::new(&pkg.name),
                        module,
                        namespace: Namespace::Value,
                        name,
                    },
                    encoding,
                )
            }),
            Resolution::System { module, name } => snapshot.package(&module).and_then(|pkg| {
                library_def_site(snapshot, &pkg, &pkg.root, &name)
                    .and_then(|(abs, span)| library_type_item(&abs, span, &name, encoding))
            }),
            Resolution::Using { module, name } => using_def_site(model, snapshot, &module, &name)
                .and_then(|(abs, span)| library_type_item(&abs, span, &name, encoding)),
            Resolution::Unresolved => None,
        };
        Some(target.into_iter().collect())
    }));
    // A racing write (`Err`) answers `None` — there is no request-side text
    // for a pure fallback.
    supers.unwrap_or_default()
}

/// The item for a library type's on-disk declaration: read and parse the
/// target file, re-derive the full declaration shape at the harvested name
/// span, and fall back to the bare span when the shape is unrecognized (e.g.
/// a macro-generated type the harvester understood but `type_decl_def` does
/// not; STRUCT is the less wrong kind for an unknown declaration form).
fn library_type_item(
    abs: &Path,
    span: Span,
    name: &str,
    encoding: PositionEncoding,
) -> Option<TypeHierarchyItem> {
    let text = std::fs::read_to_string(abs).ok()?;
    let root = parse(&text).cst;
    let uri = from_path(abs)?;
    let line_index = LineIndex::new(&text);
    let start = TextSize::new(span.start);
    if let Some((_, decl)) = type_decl_at(&root, start, &text) {
        return Some(item_for(&uri, &decl, &line_index, encoding));
    }
    let range = to_range(
        TextRange::new(start, TextSize::new(span.end)),
        &line_index,
        encoding,
    );
    Some(TypeHierarchyItem {
        name: name.to_string(),
        kind: SymbolKind::STRUCT,
        tags: None,
        detail: None,
        uri,
        range,
        selection_range: range,
        data: None,
    })
}

// ---------------------------------------------------------------------------
// Subtypes
// ---------------------------------------------------------------------------

/// The declared subtypes of `item`, re-derived like supertypes. A workspace
/// type's non-definition sites come from the reverse-occurrence index; each
/// site that is syntactically a declared-supertype position contributes its
/// enclosing declaration. `None` when the item cannot be re-derived — which
/// includes a library item's untracked depot file: subtypes of library types
/// are deferred (the reverse index only covers workspace members).
pub(crate) fn subtypes_via_db(
    snapshot: &Analysis,
    item: &TypeHierarchyItem,
    encoding: PositionEncoding,
) -> Option<Vec<TypeHierarchyItem>> {
    let path = to_path(&item.uri)?;
    let subs = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(&path)?;
        let text = snapshot.file_text(file);
        let line_index = LineIndex::new(text);
        let offset =
            TextSize::new(line_index.position_to_byte(item.selection_range.start, encoding) as u32);
        let model = snapshot.semantic_model(file);
        if let Some(symbol) = cross_file::workspace_symbol_at(snapshot, &path, model, offset)
            && symbol.namespace == Namespace::Value
        {
            return Some(workspace_subtypes(snapshot, &symbol, encoding));
        }
        // Intra-file: the declared-supertype sites among the binding's
        // occurrences. Occurrences come in source order; no dedup is needed —
        // one declaration declares one supertype.
        let binding = binding_at_cursor(model, offset)?;
        if model.binding(binding).kind != BindingKind::Type {
            return None;
        }
        let root = snapshot.parsed_tree(file);
        Some(
            model
                .occurrences(binding)
                .filter(|o| !o.is_def)
                .filter_map(|o| supertype_site_decl(&root, o.range, text))
                .map(|decl| item_for(&item.uri, &decl, &line_index, encoding))
                .collect(),
        )
    }));
    // A racing write (`Err`) answers `None` — there is no request-side text
    // for a pure fallback.
    subs.unwrap_or_default()
}

/// The declared subtypes of a workspace type, drawn from the
/// reverse-occurrence index: per member file, keep the non-definition sites
/// that are syntactically declared-supertype positions and materialize each
/// site's enclosing declaration.
fn workspace_subtypes(
    snapshot: &Analysis,
    symbol: &OccurrenceKey,
    encoding: PositionEncoding,
) -> Vec<TypeHierarchyItem> {
    let index = snapshot.workspace_reference_index();
    let Some(bucket) = index.0.get(symbol) else {
        return Vec::new();
    };
    let mut by_file: std::collections::HashMap<SourceFile, Vec<TextRange>> =
        std::collections::HashMap::new();
    for &(file, rec) in bucket {
        if !rec.is_def {
            by_file.entry(file).or_default().push(rec.range);
        }
    }
    // Deterministic order: by file path, then position within each file.
    let mut files: Vec<(SourceFile, Vec<TextRange>)> = by_file.into_iter().collect();
    files.sort_by_key(|&(file, _)| snapshot.file_path_of(file));

    let mut out = Vec::new();
    for (file, mut ranges) in files {
        let Some(path) = snapshot.file_path_of(file) else {
            continue;
        };
        let Some(uri) = from_path(&path) else {
            continue;
        };
        let text = snapshot.file_text_of(file);
        let root = snapshot.parsed_tree(file);
        let line_index = LineIndex::new(text);
        ranges.sort_by_key(|range| range.start());
        out.extend(
            ranges
                .into_iter()
                .filter_map(|range| supertype_site_decl(&root, range, text))
                .map(|decl| item_for(&uri, &decl, &line_index, encoding)),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    use crate::incremental::IncrementalDatabase;
    use crate::text::PositionEncoding::Utf16;

    fn doc_uri() -> Uri {
        Uri::from_str("file:///work/s.jl").unwrap()
    }

    /// The position of the `|` marker in `marked` (stripped before parsing).
    fn cursor(marked: &str) -> (String, Position) {
        let offset = marked.find('|').expect("a cursor marker");
        let src = marked.replacen('|', "", 1);
        let line_index = LineIndex::new(&src);
        let position = line_index.byte_to_position(offset, Utf16);
        (src, position)
    }

    fn prepare(marked: &str) -> Option<Vec<TypeHierarchyItem>> {
        let (src, position) = cursor(marked);
        compute_prepare_type_hierarchy(&doc_uri(), &src, position, Utf16)
    }

    /// A single-document db plus the item prepared at the `|` marker, for
    /// driving supertypes/subtypes through the via-db paths.
    fn db_and_item(marked: &str) -> (IncrementalDatabase, String, TypeHierarchyItem) {
        let (src, position) = cursor(marked);
        let path = Path::new("/work/s.jl");
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, src.clone());
        let items =
            prepare_type_hierarchy_via_db(&db.snapshot(), &doc_uri(), path, &src, position, Utf16)
                .expect("the marker names a type");
        (db, src, items.into_iter().next().unwrap())
    }

    fn supertypes(marked: &str) -> Vec<TypeHierarchyItem> {
        let (db, _, item) = db_and_item(marked);
        supertypes_via_db(&db.snapshot(), &item, Utf16).expect("item re-derives")
    }

    fn subtypes(marked: &str) -> Vec<TypeHierarchyItem> {
        let (db, _, item) = db_and_item(marked);
        subtypes_via_db(&db.snapshot(), &item, Utf16).expect("item re-derives")
    }

    #[test]
    fn prepare_on_a_struct_name() {
        let items = prepare("struct Poi|nt\n    x\nend").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Point");
        assert_eq!(items[0].kind, SymbolKind::STRUCT);
        assert_eq!(items[0].detail, None);
        assert_eq!(items[0].selection_range.start, Position::new(0, 7));
        assert_eq!(items[0].range.start, Position::new(0, 0));
        assert_eq!(items[0].range.end, Position::new(2, 3));
    }

    #[test]
    fn prepare_kinds_cover_all_declaration_forms() {
        let mutable = prepare("mutable struct M| end").unwrap();
        assert_eq!(mutable[0].kind, SymbolKind::STRUCT);

        let abstract_ = prepare("abstract type A| end").unwrap();
        assert_eq!(abstract_[0].kind, SymbolKind::INTERFACE);

        // The bit count sits outside the SIGNATURE, so detail stays empty.
        let primitive = prepare("primitive type F| 8 end").unwrap();
        assert_eq!(primitive[0].kind, SymbolKind::STRUCT);
        assert_eq!(primitive[0].detail, None);
    }

    #[test]
    fn prepare_detail_carries_params_and_super() {
        let items = prepare("abstract type Bar{T} end\nstruct Fo|o{T} <: Bar{T}\nend").unwrap();
        assert_eq!(items[0].name, "Foo");
        assert_eq!(items[0].detail.as_deref(), Some("{T} <: Bar{T}"));
    }

    #[test]
    fn prepare_on_a_use_points_at_the_declaration() {
        // A constructor call and a supertype position both bind to the type.
        let call = prepare("struct Point\n    x\nend\nPoi|nt(1)").unwrap();
        assert_eq!(call[0].name, "Point");
        assert_eq!(call[0].selection_range.start, Position::new(0, 7));

        let sup = prepare("abstract type Animal end\nstruct Dog <: Ani|mal\nend").unwrap();
        assert_eq!(sup[0].name, "Animal");
        assert_eq!(sup[0].kind, SymbolKind::INTERFACE);
        assert_eq!(sup[0].selection_range.start, Position::new(0, 14));
    }

    #[test]
    fn prepare_rejects_non_types() {
        // A function, a local, a type parameter, and plain text all yield
        // nothing.
        assert!(prepare("function f|()\n    1\nend").is_none());
        assert!(prepare("function f()\n    x| = 1\nend").is_none());
        assert!(prepare("struct S{T|}\n    x::T\nend").is_none());
        assert!(prepare("1 +| 1").is_none());
    }

    /// The cached-tree path matches the re-parse path when the db's tracked
    /// buffer is the live text, and falls back when the db lags or has never
    /// seen the path (mirrors call hierarchy's equivalent).
    #[test]
    fn prepare_via_db_matches_compute_and_falls_back() {
        let (src, position) = cursor("struct Point\n    x\nend\nPoi|nt(1)");
        let path = Path::new("/work/s.jl");
        let expected = compute_prepare_type_hierarchy(&doc_uri(), &src, position, Utf16);
        assert!(expected.is_some(), "fixture must yield an item");

        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, src.clone());
        assert_eq!(
            prepare_type_hierarchy_via_db(&db.snapshot(), &doc_uri(), path, &src, position, Utf16),
            expected,
            "cached-tree prepare must match the re-parse path"
        );

        let mut stale = IncrementalDatabase::default();
        stale.upsert_file(path, "y = 1\n".to_string());
        assert_eq!(
            prepare_type_hierarchy_via_db(
                &stale.snapshot(),
                &doc_uri(),
                path,
                &src,
                position,
                Utf16
            ),
            expected,
            "version skew must fall back to the buffer text"
        );
    }

    #[test]
    fn supertypes_of_a_struct_with_a_declared_super() {
        let supers = supertypes("abstract type Animal end\nstruct Do|g <: Animal\nend");
        assert_eq!(supers.len(), 1, "{supers:#?}");
        assert_eq!(supers[0].name, "Animal");
        assert_eq!(supers[0].kind, SymbolKind::INTERFACE);
        assert_eq!(supers[0].selection_range.start, Position::new(0, 14));
    }

    #[test]
    fn supertypes_are_empty_for_implicit_any() {
        // No declared supertype means `Any` — a hierarchy root, no item.
        assert!(supertypes("struct P|oint\n    x\nend").is_empty());
    }

    #[test]
    fn supertypes_unwrap_a_parametric_super() {
        let supers = supertypes("abstract type Tree{T} end\nstruct Le|af{T} <: Tree{T}\nend");
        assert_eq!(supers.len(), 1, "{supers:#?}");
        assert_eq!(supers[0].name, "Tree");
        assert_eq!(supers[0].detail.as_deref(), Some("{T}"));
    }

    #[test]
    fn supertypes_of_abstract_and_primitive_declarations() {
        let abstract_ = supertypes("abstract type A end\nabstract type B| <: A end");
        assert_eq!(abstract_.len(), 1);
        assert_eq!(abstract_[0].name, "A");

        let primitive = supertypes("abstract type A end\nprimitive type F| <: A 32 end");
        assert_eq!(primitive.len(), 1);
        assert_eq!(primitive[0].name, "A");
    }

    #[test]
    fn supertypes_defer_qualified_supers() {
        // `Base.Number` is a qualified path: deferred, not misresolved.
        assert!(supertypes("struct T| <: Base.Number\nend").is_empty());
    }

    #[test]
    fn supertypes_resolve_using_exports_into_the_library() {
        // A harvested on-disk package with a known root: a `using`'d supertype
        // materializes to its depot declaration, full shape re-derived from
        // the target file's parse.
        use std::collections::BTreeMap;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!("fatou-th-{}-{}", std::process::id(), n));
        let entry = tmp.join("src").join("Creatures.jl");
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::write(
            &entry,
            "module Creatures\nexport Beast\nabstract type Beast end\nend\n",
        )
        .unwrap();

        let pkg = crate::index::harvest_package_named(&tmp, "Creatures");
        let mut packages = BTreeMap::new();
        packages.insert("Creatures".to_string(), Arc::new(pkg));
        let mut roots = BTreeMap::new();
        roots.insert("Creatures".to_string(), tmp.clone());

        let (src, position) = cursor("using Creatures\nstruct Do|g <: Beast\nend\n");
        let path = Path::new("/work/s.jl");
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, src.clone());
        db.set_library(packages, roots, Vec::new());
        let snapshot = db.snapshot();
        let item =
            prepare_type_hierarchy_via_db(&snapshot, &doc_uri(), path, &src, position, Utf16)
                .unwrap()
                .remove(0);

        let supers = supertypes_via_db(&snapshot, &item, Utf16).unwrap();
        assert_eq!(supers.len(), 1, "{supers:#?}");
        assert_eq!(supers[0].name, "Beast");
        assert_eq!(supers[0].kind, SymbolKind::INTERFACE);
        assert_eq!(super::to_path(&supers[0].uri), Some(entry));
        // The full declaration on line 2 of the depot source.
        assert_eq!(supers[0].selection_range.start, Position::new(2, 14));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn subtypes_find_declared_subtypes_and_skip_plain_uses() {
        // Dog and Cat declare Animal as supertype; the annotation, the return
        // read, the curly type-param bound, and the `where` bound do not.
        let subs = subtypes(
            "abstract type Ani|mal end\n\
             struct Dog <: Animal\n    x\nend\n\
             struct Cat <: Animal end\n\
             f(a::Animal) = Animal\n\
             struct Pen{T<:Animal}\n    y\nend\n\
             g(x::T) where {T<:Animal} = x\n",
        );
        assert_eq!(
            subs.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
            vec!["Dog", "Cat"],
            "{subs:#?}"
        );
        assert_eq!(subs[0].kind, SymbolKind::STRUCT);
        assert_eq!(subs[0].detail.as_deref(), Some("<: Animal"));
        assert_eq!(subs[0].selection_range.start, Position::new(1, 7));
    }

    #[test]
    fn subtypes_include_parametric_declarations() {
        // `Leaf{T} <: Tree{T}`: the CURLY base counts; the argument use in
        // `h` does not.
        let subs = subtypes(
            "abstract type Tre|e{T} end\nstruct Leaf{T} <: Tree{T}\nend\nh(x::Tree{Int}) = x\n",
        );
        assert_eq!(subs.len(), 1, "{subs:#?}");
        assert_eq!(subs[0].name, "Leaf");
    }

    #[test]
    fn subtypes_exclude_supertype_argument_positions() {
        // In `struct S <: Tree{Animal}`, `Animal` is a type argument of the
        // supertype application, not a declared supertype.
        let subs = subtypes(
            "abstract type Ani|mal end\nabstract type Tree{T} end\nstruct S <: Tree{Animal}\nend\n",
        );
        assert!(subs.is_empty(), "{subs:#?}");
    }

    #[test]
    fn subtypes_are_empty_when_none_declared() {
        assert!(subtypes("abstract type Ani|mal end\nf(a::Animal) = a\n").is_empty());
    }

    #[test]
    fn cross_file_prepare_points_into_the_defining_file() {
        use crate::lsp::cross_file::test_support::{member_path, workspace_db};

        let a_text = "abstract type Animal end\n";
        let b_text = "struct Dog <: Animal\nend\n";
        let (db, _) = workspace_db(&["Animal", "Dog"], &[("a.jl", a_text), ("b.jl", b_text)]);
        let snapshot = db.snapshot();
        let b_path = member_path("b.jl");
        let b_uri = crate::lsp::uri::from_path(&b_path).unwrap();
        let a_uri = crate::lsp::uri::from_path(&member_path("a.jl")).unwrap();

        // Cursor on the `Animal` supertype use in b.jl (a free read resolving
        // to MyPkg).
        let items = prepare_type_hierarchy_via_db(
            &snapshot,
            &b_uri,
            &b_path,
            b_text,
            Position::new(0, 14),
            Utf16,
        )
        .expect("Animal resolves to the workspace");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Animal");
        assert_eq!(items[0].kind, SymbolKind::INTERFACE);
        assert_eq!(items[0].uri, a_uri);
        assert_eq!(items[0].selection_range.start, Position::new(0, 14));
    }

    #[test]
    fn cross_file_subtypes_find_declarations_in_sibling_files() {
        use crate::lsp::cross_file::test_support::{member_path, workspace_db};

        let a_text = "abstract type Animal end\n";
        let b_text = "struct Dog <: Animal\nend\n";
        let c_text = "feed(a::Animal) = a\n";
        let (db, _) = workspace_db(
            &["Animal", "Dog", "feed"],
            &[("a.jl", a_text), ("b.jl", b_text), ("c.jl", c_text)],
        );
        let snapshot = db.snapshot();
        let a_path = member_path("a.jl");
        let a_uri = crate::lsp::uri::from_path(&a_path).unwrap();
        let b_uri = crate::lsp::uri::from_path(&member_path("b.jl")).unwrap();

        let item = prepare_type_hierarchy_via_db(
            &snapshot,
            &a_uri,
            &a_path,
            a_text,
            Position::new(0, 14),
            Utf16,
        )
        .unwrap()
        .remove(0);

        // Only the declaration in b.jl counts; c.jl's annotation is a plain
        // use.
        let subs = subtypes_via_db(&snapshot, &item, Utf16).unwrap();
        assert_eq!(subs.len(), 1, "{subs:#?}");
        assert_eq!(subs[0].name, "Dog");
        assert_eq!(subs[0].uri, b_uri);
        assert_eq!(subs[0].selection_range.start, Position::new(0, 7));
    }

    #[test]
    fn cross_file_supertypes_target_the_sibling_declaration() {
        use crate::lsp::cross_file::test_support::{member_path, workspace_db};

        let a_text = "abstract type Animal end\n";
        let b_text = "struct Dog <: Animal\nend\n";
        let (db, _) = workspace_db(&["Animal", "Dog"], &[("a.jl", a_text), ("b.jl", b_text)]);
        let snapshot = db.snapshot();
        let b_path = member_path("b.jl");
        let b_uri = crate::lsp::uri::from_path(&b_path).unwrap();
        let a_uri = crate::lsp::uri::from_path(&member_path("a.jl")).unwrap();

        let item = prepare_type_hierarchy_via_db(
            &snapshot,
            &b_uri,
            &b_path,
            b_text,
            Position::new(0, 7),
            Utf16,
        )
        .expect("Dog is a workspace symbol")
        .remove(0);
        assert_eq!(item.name, "Dog");

        let supers = supertypes_via_db(&snapshot, &item, Utf16).unwrap();
        assert_eq!(supers.len(), 1, "{supers:#?}");
        assert_eq!(supers[0].name, "Animal");
        assert_eq!(supers[0].uri, a_uri);
        assert_eq!(supers[0].selection_range.start, Position::new(0, 14));
    }

    #[test]
    fn constructor_defs_do_not_shadow_the_declaration() {
        use crate::lsp::cross_file::test_support::{member_path, workspace_db};

        // a.jl holds both the struct and an outer constructor sharing its
        // name; prepare from a sibling's use still lands on the declaration.
        let a_text = "struct Foo\n    x\nend\nFoo() = Foo(1)\n";
        let b_text = "make() = Foo()\n";
        let (db, _) = workspace_db(&["Foo", "make"], &[("a.jl", a_text), ("b.jl", b_text)]);
        let snapshot = db.snapshot();
        let b_path = member_path("b.jl");
        let b_uri = crate::lsp::uri::from_path(&b_path).unwrap();
        let a_uri = crate::lsp::uri::from_path(&member_path("a.jl")).unwrap();

        let items = prepare_type_hierarchy_via_db(
            &snapshot,
            &b_uri,
            &b_path,
            b_text,
            Position::new(0, 10),
            Utf16,
        )
        .expect("Foo resolves to the workspace");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].uri, a_uri);
        assert_eq!(items[0].kind, SymbolKind::STRUCT);
        // The struct declaration on line 0, not the constructor on line 3.
        assert_eq!(items[0].selection_range.start, Position::new(0, 7));
    }
}
