//! Call hierarchy (`textDocument/prepareCallHierarchy`,
//! `callHierarchy/incomingCalls`, `callHierarchy/outgoingCalls`).
//!
//! Prepare classifies the symbol at the cursor exactly as references does — a
//! workspace top-level function through [`cross_file::workspace_symbol_at`], or
//! an intra-file [`BindingKind::Function`] binding — and returns one item per
//! function *name* (methods share a binding and an occurrence key; per-method
//! items are a later refinement). Incoming calls ride the reverse-occurrence
//! index: every non-definition site of the function is kept iff it is
//! syntactically a call ([`is_call_site`], re-derived from the cached CST — the
//! index does not record call-ness) and attributed to its nearest *named*
//! enclosing callable ([`enclosing_container`]; anonymous functions and `do`
//! blocks are walked past, a top-level call synthesizes a module or file
//! caller so nothing is dropped). Outgoing calls walk the definition's subtree
//! (nested named callables own their own calls and are skipped) and resolve
//! each callee through the shared masking order: intra-file bindings and
//! workspace siblings resolve off tracked text, Base/depot targets materialize
//! their harvested [`DefLocation`](crate::index::model::DefLocation) from disk
//! like go-to-definition does.
//!
//! Incoming and outgoing receive a [`CallHierarchyItem`], not a live document
//! position: the target is re-derived from `item.uri` plus
//! `item.selection_range` against the snapshot's *tracked* text, which works
//! for closed member files (members are disk-seeded). On position skew or a
//! racing write (`salsa::Cancelled`) they return `None` rather than answering
//! from a stale tree — clients re-prepare after an edit.
//!
//! Deferred: macro calls (`@foo`), qualified-call sites (`Pkg.foo(x)`,
//! matching `workspace_occurrences`' deferral), per-method items under
//! multiple dispatch, and incoming calls for library symbols (the reverse
//! index only covers workspace members).

use std::collections::{BTreeMap, HashMap};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};

use lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyOutgoingCall, Position, Range,
    SymbolKind, Uri,
};
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
use super::symbols::{
    callee_name, head_name, op_token, signature_detail, signature_expr, token_at, unwrap_head,
};
use super::uri::{from_path, to_path};

/// A named callable definition extracted from the CST: a long-form
/// `function`/`macro` definition or a short-form `f(x) = ...` assignment.
struct Callable {
    /// The defined name (`@m` for macros, `Base.show` for qualified extensions).
    name: String,
    /// The rest of the signature, as document symbols render it.
    detail: Option<String>,
    /// The whole definition node.
    range: TextRange,
    /// The name within it.
    selection: TextRange,
}

/// The callable definition `node` introduces, when it is one. Mirrors the
/// shapes document symbols recognize (`def_symbol`/`short_form_symbol` in
/// [`symbols`](super::symbols)): a `FUNCTION_DEF`/`MACRO_DEF`, or an
/// `ASSIGNMENT_EXPR` whose LHS (under `where`/`::` wrappers) is a call and
/// whose operator is a plain `=`.
fn callable_def(node: &SyntaxNode, text: &str) -> Option<Callable> {
    match node.kind() {
        SyntaxKind::FUNCTION_DEF | SyntaxKind::MACRO_DEF => {
            let sig_expr = signature_expr(node)?;
            let head = unwrap_head(sig_expr.clone());
            let (mut name, selection) = if head.kind() == SyntaxKind::CALL_EXPR {
                callee_name(&head)?
            } else {
                // `function f end`: a bare name with no parameter list.
                head_name(&head)?
            };
            if node.kind() == SyntaxKind::MACRO_DEF {
                name.insert(0, '@');
            }
            let detail = signature_detail(&sig_expr, selection, text);
            Some(Callable {
                name,
                detail,
                range: node.text_range(),
                selection,
            })
        }
        SyntaxKind::ASSIGNMENT_EXPR => {
            if op_token(node)?.kind() != SyntaxKind::EQ {
                return None;
            }
            let lhs = node.children().next()?;
            let head = unwrap_head(lhs.clone());
            if head.kind() != SyntaxKind::CALL_EXPR {
                return None;
            }
            let (name, selection) = callee_name(&head)?;
            let detail = signature_detail(&lhs, selection, text);
            Some(Callable {
                name,
                detail,
                range: node.text_range(),
                selection,
            })
        }
        _ => None,
    }
}

/// The callable definition whose *name* spans `offset`, with its node: what
/// prepare returns and what a returned item's `selection_range` re-derives.
fn callable_at(root: &SyntaxNode, offset: TextSize, text: &str) -> Option<(SyntaxNode, Callable)> {
    let token = token_at(root, offset)?;
    token.parent_ancestors().find_map(|node| {
        let callable = callable_def(&node, text)?;
        callable
            .selection
            .contains_inclusive(offset)
            .then_some((node, callable))
    })
}

/// What lexically contains a call site: the nearest *named* callable
/// (anonymous functions and `do` blocks are walked past), else the nearest
/// inline `module`, else the file itself — so a top-level call is attributed
/// rather than dropped.
enum Container {
    Callable(Callable),
    Module {
        name: String,
        range: TextRange,
        selection: TextRange,
    },
    File,
}

fn enclosing_container(root: &SyntaxNode, offset: TextSize, text: &str) -> Container {
    let Some(token) = token_at(root, offset) else {
        return Container::File;
    };
    for node in token.parent_ancestors() {
        if let Some(callable) = callable_def(&node, text) {
            return Container::Callable(callable);
        }
        if node.kind() == SyntaxKind::MODULE_DEF
            && let Some((name, selection)) =
                signature_expr(&node).and_then(|e| head_name(&unwrap_head(e)))
        {
            return Container::Module {
                name,
                range: node.text_range(),
                selection,
            };
        }
    }
    Container::File
}

/// Whether the identifier occurrence spanning `range` is syntactically a call:
/// the callee of a `CALL_EXPR` or broadcast `DOT_CALL_EXPR` that is not itself
/// a definition signature (`function f(x)` and `f(x) = ...` introduce `f`,
/// they do not call it). The semantic model does not record call-ness, so it
/// is re-derived here from the (cached) CST.
fn is_call_site(root: &SyntaxNode, range: TextRange) -> bool {
    let Some(token) = token_at(root, range.start()) else {
        return false;
    };
    // The occurrence covers a NAME (or `var"..."`) node; its parent is the
    // candidate call.
    let Some(name) = token.parent_ancestors().find(|n| n.text_range() == range) else {
        return false;
    };
    let Some(call) = name.parent() else {
        return false;
    };
    if !matches!(
        call.kind(),
        SyntaxKind::CALL_EXPR | SyntaxKind::DOT_CALL_EXPR
    ) {
        return false;
    }
    // The callee is the call's head; an occurrence elsewhere in the call (an
    // argument, like the `f` of `map(f, xs)`) is a plain read.
    if callee_name(&call).map(|(_, selection)| selection) != Some(range) {
        return false;
    }
    !is_definition_signature(&call)
}

/// Whether `call` is the signature of a definition rather than an invocation:
/// under a `SIGNATURE` (long form), or — through `where`/`::` wrappers — the
/// LHS of a plain-`=` assignment (short form).
fn is_definition_signature(call: &SyntaxNode) -> bool {
    let mut node = call.clone();
    loop {
        let Some(parent) = node.parent() else {
            return false;
        };
        match parent.kind() {
            SyntaxKind::WHERE_EXPR | SyntaxKind::TYPE_ANNOTATION => node = parent,
            SyntaxKind::SIGNATURE => return true,
            SyntaxKind::ASSIGNMENT_EXPR => {
                return parent.first_child().as_ref() == Some(&node)
                    && op_token(&parent).is_some_and(|t| t.kind() == SyntaxKind::EQ);
            }
            _ => return false,
        }
    }
}

fn to_range(range: TextRange, line_index: &LineIndex, encoding: PositionEncoding) -> Range {
    Range {
        start: line_index.byte_to_position(range.start().into(), encoding),
        end: line_index.byte_to_position(range.end().into(), encoding),
    }
}

/// The item for a callable definition. Kind is always `FUNCTION` (macros keep
/// the document-symbols convention: `@name`, no macro kind in LSP).
fn item_for(
    uri: &Uri,
    callable: &Callable,
    line_index: &LineIndex,
    encoding: PositionEncoding,
) -> CallHierarchyItem {
    CallHierarchyItem {
        name: callable.name.clone(),
        kind: SymbolKind::FUNCTION,
        tags: None,
        detail: callable.detail.clone(),
        uri: uri.clone(),
        range: to_range(callable.range, line_index, encoding),
        selection_range: to_range(callable.selection, line_index, encoding),
        data: None,
    }
}

/// The caller item for a [`Container`]: the named callable itself, or a
/// synthesized module/file item for top-level call sites.
fn container_item(
    container: &Container,
    uri: &Uri,
    path: &Path,
    root: &SyntaxNode,
    line_index: &LineIndex,
    encoding: PositionEncoding,
) -> CallHierarchyItem {
    match container {
        Container::Callable(callable) => item_for(uri, callable, line_index, encoding),
        Container::Module {
            name,
            range,
            selection,
        } => CallHierarchyItem {
            name: name.clone(),
            kind: SymbolKind::MODULE,
            tags: None,
            detail: None,
            uri: uri.clone(),
            range: to_range(*range, line_index, encoding),
            selection_range: to_range(*selection, line_index, encoding),
            data: None,
        },
        Container::File => CallHierarchyItem {
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| uri.to_string()),
            kind: SymbolKind::FILE,
            tags: None,
            detail: None,
            uri: uri.clone(),
            range: to_range(root.text_range(), line_index, encoding),
            selection_range: Range::new(Position::new(0, 0), Position::new(0, 0)),
            data: None,
        },
    }
}

// ---------------------------------------------------------------------------
// Prepare
// ---------------------------------------------------------------------------

/// The call-hierarchy item for the function at `position` in `text`,
/// re-parsing it. Pure and unit-testable; single-file, so a free read (a
/// library or workspace symbol) yields `None` here — the via-db path resolves
/// those through the reverse-occurrence index.
pub fn compute_prepare_call_hierarchy(
    uri: &Uri,
    text: &str,
    position: Position,
    encoding: PositionEncoding,
) -> Option<Vec<CallHierarchyItem>> {
    let root = parse(text).cst;
    let model = SemanticModel::build(&root);
    let line_index = LineIndex::new(text);
    let offset = TextSize::new(line_index.position_to_byte(position, encoding) as u32);
    prepare_for(&root, &model, uri, text, &line_index, offset, encoding)
}

/// The intra-file classification shared by both prepare paths: the cursor
/// names a [`BindingKind::Function`] binding (its definition site or a call),
/// whose definition supplies the item.
fn prepare_for(
    root: &SyntaxNode,
    model: &SemanticModel,
    uri: &Uri,
    text: &str,
    line_index: &LineIndex,
    offset: TextSize,
    encoding: PositionEncoding,
) -> Option<Vec<CallHierarchyItem>> {
    let binding = binding_at_cursor(model, offset)?;
    let binding = model.binding(binding);
    if binding.kind != BindingKind::Function {
        return None;
    }
    let (_, callable) = callable_at(root, binding.def_range.start(), text)?;
    Some(vec![item_for(uri, &callable, line_index, encoding)])
}

/// Prepare off the snapshot's cached parse when the db's tracked buffer for
/// `path` still matches `text`; otherwise re-parse. A workspace top-level
/// function resolves through the reverse-occurrence index to its defining
/// file; anything else stays intra-file. Mirrors
/// [`references_via_db`](super::references::references_via_db).
pub(crate) fn prepare_call_hierarchy_via_db(
    snapshot: &Analysis,
    uri: &Uri,
    path: &Path,
    text: &str,
    position: Position,
    encoding: PositionEncoding,
) -> Option<Vec<CallHierarchyItem>> {
    let line_index = LineIndex::new(text);
    let offset = TextSize::new(line_index.position_to_byte(position, encoding) as u32);
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            return None;
        }
        let model = snapshot.semantic_model(file);
        // Macros are deferred; a macro key falls through to the intra-file
        // path, whose Function gate rejects it too.
        if let Some(symbol) = cross_file::workspace_symbol_at(snapshot, path, model, offset)
            && symbol.namespace == Namespace::Value
            && let Some(item) = workspace_item(snapshot, &symbol, encoding)
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
        Ok(None) | Err(_) => compute_prepare_call_hierarchy(uri, text, position, encoding),
    }
}

/// The item for a workspace symbol's first definition site (by file, then
/// position — the "first method" rule go-to-definition also uses), built
/// through the defining file's tracked text. `None` when no definition rec is
/// a callable definition — a const or type also keys the index, and call
/// hierarchy is for functions.
fn workspace_item(
    snapshot: &Analysis,
    symbol: &OccurrenceKey,
    encoding: PositionEncoding,
) -> Option<CallHierarchyItem> {
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
        if let Some((_, callable)) = callable_at(&root, range.start(), text) {
            let line_index = LineIndex::new(text);
            return Some(item_for(&uri, &callable, &line_index, encoding));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Incoming calls
// ---------------------------------------------------------------------------

/// The incoming calls to `item`, re-derived from its `uri` plus
/// `selection_range` against the snapshot's tracked text (which works for
/// closed member files — members are disk-seeded). `None` when the item cannot
/// be re-derived (a stale position after an edit, an untracked file) or a
/// write races the read — never wrong data; there is no request-side text for
/// a pure fallback.
pub(crate) fn incoming_calls_via_db(
    snapshot: &Analysis,
    item: &CallHierarchyItem,
    encoding: PositionEncoding,
) -> Option<Vec<CallHierarchyIncomingCall>> {
    // A synthesized module/file caller does not expand upward.
    if item.kind != SymbolKind::FUNCTION {
        return Some(Vec::new());
    }
    let path = to_path(&item.uri)?;
    let calls = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(&path)?;
        let text = snapshot.file_text(file);
        let line_index = LineIndex::new(text);
        let offset =
            TextSize::new(line_index.position_to_byte(item.selection_range.start, encoding) as u32);
        let model = snapshot.semantic_model(file);
        if let Some(symbol) = cross_file::workspace_symbol_at(snapshot, &path, model, offset)
            && symbol.namespace == Namespace::Value
        {
            return Some(workspace_incoming(snapshot, &symbol, encoding));
        }
        // Intra-file: the call sites among the binding's occurrences.
        let binding = binding_at_cursor(model, offset)?;
        if model.binding(binding).kind != BindingKind::Function {
            return None;
        }
        let root = snapshot.parsed_tree(file);
        let sites: Vec<TextRange> = model
            .occurrences(binding)
            .filter(|o| !o.is_def && is_call_site(&root, o.range))
            .map(|o| o.range)
            .collect();
        Some(group_incoming(
            &root, text, &item.uri, &path, &sites, encoding,
        ))
    }));
    // A racing write (`Err`) answers `None` — there is no request-side text
    // for a pure fallback.
    calls.unwrap_or_default()
}

/// The incoming calls to a workspace symbol, drawn from the reverse-occurrence
/// index: per member file, keep the non-definition sites that are syntactically
/// calls and attribute each to its enclosing container.
fn workspace_incoming(
    snapshot: &Analysis,
    symbol: &OccurrenceKey,
    encoding: PositionEncoding,
) -> Vec<CallHierarchyIncomingCall> {
    let index = snapshot.workspace_reference_index();
    let Some(bucket) = index.0.get(symbol) else {
        return Vec::new();
    };
    let mut by_file: HashMap<SourceFile, Vec<TextRange>> = HashMap::new();
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
        ranges.retain(|&range| is_call_site(&root, range));
        ranges.sort_by_key(|range| range.start());
        out.extend(group_incoming(&root, text, &uri, &path, &ranges, encoding));
    }
    out
}

/// Group one file's call sites by their enclosing container into
/// `CallHierarchyIncomingCall`s (one per caller, `from_ranges` in source
/// order). `sites` must be sorted by position.
fn group_incoming(
    root: &SyntaxNode,
    text: &str,
    uri: &Uri,
    path: &Path,
    sites: &[TextRange],
    encoding: PositionEncoding,
) -> Vec<CallHierarchyIncomingCall> {
    let line_index = LineIndex::new(text);
    // Keyed by the caller's selection range: same range, same caller. A
    // BTreeMap keeps callers in position order.
    let mut groups: BTreeMap<(u32, u32), CallHierarchyIncomingCall> = BTreeMap::new();
    for &site in sites {
        let container = enclosing_container(root, site.start(), text);
        let from = container_item(&container, uri, path, root, &line_index, encoding);
        let key = match &container {
            Container::Callable(c) => (c.selection.start().into(), c.selection.end().into()),
            Container::Module { selection, .. } => {
                (selection.start().into(), selection.end().into())
            }
            Container::File => (0, 0),
        };
        groups
            .entry(key)
            .or_insert_with(|| CallHierarchyIncomingCall {
                from,
                from_ranges: Vec::new(),
            })
            .from_ranges
            .push(to_range(site, &line_index, encoding));
    }
    groups.into_values().collect()
}

// ---------------------------------------------------------------------------
// Outgoing calls
// ---------------------------------------------------------------------------

/// The outgoing calls from `item`, re-derived like incoming. The definition's
/// subtree is walked for call sites (nested named callables and modules own
/// their own calls) and each callee resolves through the shared masking order:
/// an intra-file binding or a workspace sibling resolves off tracked text; a
/// Base/depot function materializes its harvested location from disk (one read
/// and parse per distinct target file, cached within the request). A
/// synthesized module/file item reports its top-level calls.
pub(crate) fn outgoing_calls_via_db(
    snapshot: &Analysis,
    item: &CallHierarchyItem,
    encoding: PositionEncoding,
) -> Option<Vec<CallHierarchyOutgoingCall>> {
    let path = to_path(&item.uri)?;
    let calls = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(&path)?;
        let text = snapshot.file_text(file);
        let line_index = LineIndex::new(text);
        let offset =
            TextSize::new(line_index.position_to_byte(item.selection_range.start, encoding) as u32);
        let root = snapshot.parsed_tree(file);
        // The subtree whose calls belong to this item.
        let source = match item.kind {
            SymbolKind::FILE => root.clone(),
            SymbolKind::MODULE => token_at(&root, offset)?
                .parent_ancestors()
                .find(|n| n.kind() == SyntaxKind::MODULE_DEF)?,
            _ => callable_at(&root, offset, text)?.0,
        };
        let mut sites: Vec<(String, TextRange)> = Vec::new();
        collect_call_sites(&source, text, &mut sites);

        let model = snapshot.semantic_model(file);
        let workspace = snapshot.workspace_member(&path);
        let resolver = Resolver::new(model, snapshot).with_workspace(workspace.clone());
        // One read+parse per distinct library file within this request.
        let mut parsed_files: HashMap<PathBuf, Option<(String, SyntaxNode)>> = HashMap::new();

        // Keyed by (target uri, target selection): one entry per callee, with
        // every call site of it in this body unioned into `from_ranges`.
        let mut groups: BTreeMap<(String, u32, u32), CallHierarchyOutgoingCall> = BTreeMap::new();
        for (name, range) in sites {
            let target = match resolver.resolve(&name, range.start(), Namespace::Value) {
                Resolution::Binding(bid) => {
                    let binding = model.binding(bid);
                    // Imports are not chased into the library (matching hover
                    // and go-to-definition); locals and consts are not calls'
                    // targets in the hierarchy.
                    if binding.kind != BindingKind::Function {
                        None
                    } else {
                        callable_at(&root, binding.def_range.start(), text).map(|(_, callable)| {
                            item_for(&item.uri, &callable, &line_index, encoding)
                        })
                    }
                }
                Resolution::Workspace { module, name } => {
                    workspace.as_ref().and_then(|(pkg, _)| {
                        workspace_item(
                            snapshot,
                            &OccurrenceKey {
                                package: smol_str::SmolStr::new(&pkg.name),
                                module,
                                namespace: Namespace::Value,
                                name,
                            },
                            encoding,
                        )
                    })
                }
                Resolution::System { module, name } => snapshot.package(&module).and_then(|pkg| {
                    library_def_site(snapshot, &pkg, &pkg.root, &name).and_then(|(abs, span)| {
                        library_item(&abs, span, &name, encoding, &mut parsed_files)
                    })
                }),
                Resolution::Using { module, name } => {
                    using_def_site(model, snapshot, &module, &name).and_then(|(abs, span)| {
                        library_item(&abs, span, &name, encoding, &mut parsed_files)
                    })
                }
                Resolution::Unresolved => None,
            };
            let Some(target) = target else {
                continue;
            };
            let key = (
                target.uri.to_string(),
                target.selection_range.start.line,
                target.selection_range.start.character,
            );
            groups
                .entry(key)
                .or_insert_with(|| CallHierarchyOutgoingCall {
                    to: target,
                    from_ranges: Vec::new(),
                })
                .from_ranges
                .push(to_range(range, &line_index, encoding));
        }
        Some(groups.into_values().collect())
    }));
    // A racing write (`Err`) answers `None` — there is no request-side text
    // for a pure fallback.
    calls.unwrap_or_default()
}

/// Collect `(callee name, name range)` for every call lexically owned by
/// `node`'s subtree, in source order. Nested named callables and nested
/// `module`s own their own calls and are not descended into; anonymous
/// functions and `do` bodies belong to the enclosing item (symmetric with
/// [`enclosing_container`]). Only plain-name callees are collected — qualified
/// (`Pkg.foo`), parametric (`Foo{T}`), and bare-operator callees are deferred.
fn collect_call_sites(node: &SyntaxNode, text: &str, out: &mut Vec<(String, TextRange)>) {
    for child in node.children() {
        if callable_def(&child, text).is_some() || child.kind() == SyntaxKind::MODULE_DEF {
            continue;
        }
        if matches!(
            child.kind(),
            SyntaxKind::CALL_EXPR | SyntaxKind::DOT_CALL_EXPR
        ) && !is_definition_signature(&child)
            && let Some(callee) = child.children().next()
            && matches!(
                callee.kind(),
                SyntaxKind::NAME | SyntaxKind::NONSTANDARD_IDENTIFIER
            )
            && let Some((name, selection)) = head_name(&callee)
        {
            out.push((name, selection));
        }
        // A call's arguments (and a signature's default values) may hold
        // further calls.
        collect_call_sites(&child, text, out);
    }
}

/// The item for a library function's on-disk definition: read and parse the
/// target file (cached per file within one request), re-derive the full
/// definition shape at the harvested name span, and fall back to the bare span
/// when the shape is unrecognized (e.g. a `ccall` wrapper the harvester
/// understood but `callable_def` does not).
fn library_item(
    abs: &Path,
    span: Span,
    name: &str,
    encoding: PositionEncoding,
    parsed_files: &mut HashMap<PathBuf, Option<(String, SyntaxNode)>>,
) -> Option<CallHierarchyItem> {
    let entry = parsed_files.entry(abs.to_path_buf()).or_insert_with(|| {
        std::fs::read_to_string(abs).ok().map(|text| {
            let root = parse(&text).cst;
            (text, root)
        })
    });
    let (text, root) = entry.as_ref()?;
    let uri = from_path(abs)?;
    let line_index = LineIndex::new(text);
    let start = TextSize::new(span.start);
    if let Some((_, callable)) = callable_at(root, start, text) {
        return Some(item_for(&uri, &callable, &line_index, encoding));
    }
    let range = to_range(
        TextRange::new(start, TextSize::new(span.end)),
        &line_index,
        encoding,
    );
    Some(CallHierarchyItem {
        name: name.to_string(),
        kind: SymbolKind::FUNCTION,
        tags: None,
        detail: None,
        uri,
        range,
        selection_range: range,
        data: None,
    })
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

    fn prepare(marked: &str) -> Option<Vec<CallHierarchyItem>> {
        let (src, position) = cursor(marked);
        compute_prepare_call_hierarchy(&doc_uri(), &src, position, Utf16)
    }

    /// A single-document db plus the item prepared at the `|` marker, for
    /// driving incoming/outgoing through the via-db paths.
    fn db_and_item(marked: &str) -> (IncrementalDatabase, String, CallHierarchyItem) {
        let (src, position) = cursor(marked);
        let path = Path::new("/work/s.jl");
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, src.clone());
        let items =
            prepare_call_hierarchy_via_db(&db.snapshot(), &doc_uri(), path, &src, position, Utf16)
                .expect("the marker names a function");
        (db, src, items.into_iter().next().unwrap())
    }

    fn incoming(marked: &str) -> Vec<CallHierarchyIncomingCall> {
        let (db, _, item) = db_and_item(marked);
        incoming_calls_via_db(&db.snapshot(), &item, Utf16).expect("item re-derives")
    }

    fn outgoing(marked: &str) -> Vec<CallHierarchyOutgoingCall> {
        let (db, _, item) = db_and_item(marked);
        outgoing_calls_via_db(&db.snapshot(), &item, Utf16).expect("item re-derives")
    }

    #[test]
    fn prepare_on_a_long_form_definition_name() {
        let items = prepare("function fo|o(x)\n    x\nend").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "foo");
        assert_eq!(items[0].kind, SymbolKind::FUNCTION);
        assert_eq!(items[0].detail.as_deref(), Some("(x)"));
        assert_eq!(items[0].selection_range.start, Position::new(0, 9));
        assert_eq!(items[0].range.start, Position::new(0, 0));
        assert_eq!(items[0].range.end, Position::new(2, 3));
    }

    #[test]
    fn prepare_on_a_short_form_definition() {
        let items = prepare("gre|et(a) = a").unwrap();
        assert_eq!(items[0].name, "greet");
        assert_eq!(items[0].selection_range.start, Position::new(0, 0));
    }

    #[test]
    fn prepare_on_a_call_site_points_at_the_definition() {
        let items = prepare("greet(a) = a\ngre|et(1)").unwrap();
        assert_eq!(items[0].name, "greet");
        // The item is the definition, not the call.
        assert_eq!(items[0].selection_range.start, Position::new(0, 0));
    }

    #[test]
    fn prepare_rejects_non_functions() {
        // A local variable, a struct name, and plain text all yield nothing.
        assert!(prepare("function f()\n    x| = 1\nend").is_none());
        assert!(prepare("struct Poi|nt\n    x\nend").is_none());
        assert!(prepare("1 +| 1").is_none());
    }

    /// The cached-tree path matches the re-parse path when the db's tracked
    /// buffer is the live text, and falls back when the db lags or has never
    /// seen the path (mirrors `symbols_via_db_match_compute_and_fall_back`).
    #[test]
    fn prepare_via_db_matches_compute_and_falls_back() {
        let (src, position) = cursor("greet(a) = a\ngre|et(1)");
        let path = Path::new("/work/s.jl");
        let expected = compute_prepare_call_hierarchy(&doc_uri(), &src, position, Utf16);
        assert!(expected.is_some(), "fixture must yield an item");

        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, src.clone());
        assert_eq!(
            prepare_call_hierarchy_via_db(&db.snapshot(), &doc_uri(), path, &src, position, Utf16),
            expected,
            "cached-tree prepare must match the re-parse path"
        );

        let mut stale = IncrementalDatabase::default();
        stale.upsert_file(path, "y = 1\n".to_string());
        assert_eq!(
            prepare_call_hierarchy_via_db(
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
    fn incoming_groups_a_callers_sites_and_keeps_top_level() {
        // `caller` calls twice (one grouped entry); the bare top-level call
        // lands on a synthesized file item; `map(f, ...)` passes `f` as a
        // value, not a call.
        let calls =
            incoming("f|(x) = x\nfunction caller()\n    f(1)\n    f(2)\nend\nmap(f, [1])\nf(3)\n");
        assert_eq!(calls.len(), 2, "{calls:#?}");

        let file = &calls[0];
        assert_eq!(file.from.kind, SymbolKind::FILE);
        assert_eq!(file.from.name, "s.jl");
        assert_eq!(file.from_ranges.len(), 1);
        assert_eq!(file.from_ranges[0].start, Position::new(6, 0));

        let caller = &calls[1];
        assert_eq!(caller.from.name, "caller");
        assert_eq!(caller.from.kind, SymbolKind::FUNCTION);
        assert_eq!(
            caller
                .from_ranges
                .iter()
                .map(|r| r.start.line)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn incoming_excludes_other_method_definitions() {
        // The second method's signature mentions `f` but defines it.
        let calls = incoming("f|(x) = x\nf(x, y) = x\nf(1)\n");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].from.kind, SymbolKind::FILE);
        assert_eq!(calls[0].from_ranges.len(), 1);
        assert_eq!(calls[0].from_ranges[0].start, Position::new(2, 0));
    }

    #[test]
    fn incoming_attributes_anonymous_functions_to_the_named_caller() {
        let calls = incoming("f|(x) = x\ng() = map(x -> f(x), [1])\n");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].from.name, "g");
    }

    #[test]
    fn incoming_counts_broadcast_and_do_block_calls() {
        let calls = incoming(
            "f|(g, x) = g(x)\nfunction h()\n    f([1]) do x\n        x\n    end\n    f.([2])\nend\n",
        );
        assert_eq!(calls.len(), 1, "{calls:#?}");
        assert_eq!(calls[0].from.name, "h");
        assert_eq!(calls[0].from_ranges.len(), 2);
    }

    #[test]
    fn incoming_attributes_module_top_level_to_the_module() {
        let calls = incoming("module M\nf|(x) = x\nf(1)\nend\n");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].from.name, "M");
        assert_eq!(calls[0].from.kind, SymbolKind::MODULE);
    }

    #[test]
    fn synthesized_containers_do_not_expand_upward() {
        // Expanding incoming on a module/file caller item returns no calls
        // rather than misclassifying whatever sits at its selection range.
        let calls = incoming("module M\nf|(x) = x\nf(1)\nend\n");
        let module_item = calls[0].from.clone();
        let (db, _, _) = db_and_item("module M\nf|(x) = x\nf(1)\nend\n");
        assert_eq!(
            incoming_calls_via_db(&db.snapshot(), &module_item, Utf16),
            Some(Vec::new())
        );
    }

    #[test]
    fn outgoing_finds_body_calls_and_skips_nested_definitions() {
        // `helper` is called from `main`'s body; `inner` is a nested named
        // callable whose own call of `other` belongs to `inner`; the
        // unresolved callee is dropped.
        let calls = outgoing(
            "helper(x) = x\nother(x) = x\nfunction ma|in()\n    helper(1)\n    inner(y) = other(y)\n    nope(2)\nend\n",
        );
        assert_eq!(calls.len(), 1, "{calls:#?}");
        assert_eq!(calls[0].to.name, "helper");
        assert_eq!(calls[0].from_ranges.len(), 1);
        assert_eq!(calls[0].from_ranges[0].start, Position::new(3, 4));
    }

    #[test]
    fn outgoing_excludes_the_own_signature_and_finds_recursion() {
        let calls = outgoing("fa|ct(n) = n <= 1 ? 1 : n * fact(n - 1)\n");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].to.name, "fact");
        // Only the recursive call, not the defining signature.
        assert_eq!(calls[0].from_ranges.len(), 1);
        assert_eq!(calls[0].from_ranges[0].start, Position::new(0, 27));
    }

    #[test]
    fn outgoing_groups_repeat_calls_to_one_target() {
        let calls = outgoing("f(x) = x\ng|() = f(1) + f(2)\n");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].to.name, "f");
        assert_eq!(calls[0].from_ranges.len(), 2);
    }

    #[test]
    fn outgoing_resolves_using_exports_into_the_library() {
        // A harvested on-disk package with a known root: a `using`'d callee
        // materializes to its depot definition, full shape re-derived from the
        // target file's parse.
        use std::collections::BTreeMap;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!("fatou-ch-{}-{}", std::process::id(), n));
        let entry = tmp.join("src").join("Greetings.jl");
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::write(
            &entry,
            "module Greetings\nexport greet\ngreet(name) = name\nend\n",
        )
        .unwrap();

        let pkg = crate::index::harvest_package_named(&tmp, "Greetings");
        let mut packages = BTreeMap::new();
        packages.insert("Greetings".to_string(), Arc::new(pkg));
        let mut roots = BTreeMap::new();
        roots.insert("Greetings".to_string(), tmp.clone());

        let (src, position) = cursor("using Greetings\ncall|it() = greet(1)\n");
        let path = Path::new("/work/s.jl");
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, src.clone());
        db.set_library(packages, roots, Vec::new());
        let snapshot = db.snapshot();
        let item =
            prepare_call_hierarchy_via_db(&snapshot, &doc_uri(), path, &src, position, Utf16)
                .unwrap()
                .remove(0);

        let calls = outgoing_calls_via_db(&snapshot, &item, Utf16).unwrap();
        assert_eq!(calls.len(), 1, "{calls:#?}");
        assert_eq!(calls[0].to.name, "greet");
        assert_eq!(super::to_path(&calls[0].to.uri), Some(entry));
        // The full short-form definition on line 2 of the depot source.
        assert_eq!(calls[0].to.selection_range.start, Position::new(2, 0));
        assert_eq!(calls[0].to.range.start, Position::new(2, 0));
        assert_eq!(calls[0].to.range.end, Position::new(2, 18));
        // The call site is reported in the *caller's* document.
        assert_eq!(calls[0].from_ranges[0].start, Position::new(1, 11));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn cross_file_prepare_points_into_the_defining_file() {
        use crate::lsp::cross_file::test_support::{member_path, workspace_db};

        let a_text = "greet(a) = a\n";
        let b_text = "callit() = greet(2)\n";
        let (db, _) = workspace_db(&["greet", "callit"], &[("a.jl", a_text), ("b.jl", b_text)]);
        let snapshot = db.snapshot();
        let b_path = member_path("b.jl");
        let b_uri = crate::lsp::uri::from_path(&b_path).unwrap();
        let a_uri = crate::lsp::uri::from_path(&member_path("a.jl")).unwrap();

        // Cursor on the `greet` call in b.jl (a free read resolving to MyPkg).
        let items = prepare_call_hierarchy_via_db(
            &snapshot,
            &b_uri,
            &b_path,
            b_text,
            Position::new(0, 11),
            Utf16,
        )
        .expect("greet resolves to the workspace");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "greet");
        assert_eq!(items[0].uri, a_uri);
        assert_eq!(items[0].selection_range.start, Position::new(0, 0));
    }

    #[test]
    fn cross_file_incoming_finds_callers_in_sibling_files() {
        use crate::lsp::cross_file::test_support::{member_path, workspace_db};

        let a_text = "greet(a) = a\n";
        let b_text = "callit() = greet(2)\n";
        let (db, _) = workspace_db(&["greet", "callit"], &[("a.jl", a_text), ("b.jl", b_text)]);
        let snapshot = db.snapshot();
        let a_path = member_path("a.jl");
        let a_uri = crate::lsp::uri::from_path(&a_path).unwrap();
        let b_uri = crate::lsp::uri::from_path(&member_path("b.jl")).unwrap();

        let item = prepare_call_hierarchy_via_db(
            &snapshot,
            &a_uri,
            &a_path,
            a_text,
            Position::new(0, 0),
            Utf16,
        )
        .unwrap()
        .remove(0);

        let calls = incoming_calls_via_db(&snapshot, &item, Utf16).unwrap();
        assert_eq!(calls.len(), 1, "{calls:#?}");
        assert_eq!(calls[0].from.name, "callit");
        assert_eq!(calls[0].from.uri, b_uri);
        assert_eq!(calls[0].from_ranges[0].start, Position::new(0, 11));
    }

    #[test]
    fn cross_file_outgoing_targets_the_sibling_definition() {
        use crate::lsp::cross_file::test_support::{member_path, workspace_db};

        let a_text = "greet(a) = a\n";
        let b_text = "callit() = greet(2)\n";
        let (db, _) = workspace_db(&["greet", "callit"], &[("a.jl", a_text), ("b.jl", b_text)]);
        let snapshot = db.snapshot();
        let b_path = member_path("b.jl");
        let b_uri = crate::lsp::uri::from_path(&b_path).unwrap();
        let a_uri = crate::lsp::uri::from_path(&member_path("a.jl")).unwrap();

        let item = prepare_call_hierarchy_via_db(
            &snapshot,
            &b_uri,
            &b_path,
            b_text,
            Position::new(0, 1),
            Utf16,
        )
        .expect("callit is a workspace symbol")
        .remove(0);
        assert_eq!(item.name, "callit");

        let calls = outgoing_calls_via_db(&snapshot, &item, Utf16).unwrap();
        assert_eq!(calls.len(), 1, "{calls:#?}");
        assert_eq!(calls[0].to.name, "greet");
        assert_eq!(calls[0].to.uri, a_uri);
        assert_eq!(calls[0].to.selection_range.start, Position::new(0, 0));
        // The call site stays in the caller's document.
        assert_eq!(calls[0].from_ranges[0].start, Position::new(0, 11));
    }

    #[test]
    fn outgoing_from_a_file_item_reports_top_level_calls() {
        // Expand incoming on `f` to get the synthesized file caller, then ask
        // for its outgoing calls: the top-level call only, not `g`'s.
        let src = "f|(x) = x\ng() = f(1)\nf(2)\n";
        let calls = incoming(src);
        let file_item = calls
            .iter()
            .find(|c| c.from.kind == SymbolKind::FILE)
            .expect("a top-level caller")
            .from
            .clone();
        let (db, _, _) = db_and_item(src);
        let out = outgoing_calls_via_db(&db.snapshot(), &file_item, Utf16).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].to.name, "f");
        assert_eq!(out[0].from_ranges.len(), 1);
        assert_eq!(out[0].from_ranges[0].start, Position::new(2, 0));
    }
}
