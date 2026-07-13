//! Document symbols (`textDocument/documentSymbol`): a hierarchical outline of
//! modules, functions (long and short form), macros, structs (with fields),
//! abstract/primitive types, and consts. A pure CST walk — Julia definitions
//! carry explicit keywords, so no semantic model is needed. The same walk later
//! feeds workspace symbols (see `TODO.md`, language server Phase 5).
//!
//! Conventions: macros render with their sigil (`@m`, kind `FUNCTION` — LSP has
//! no macro kind); functions carry the rest of the signature in `detail` (with
//! multiple dispatch, many methods share a name and the signature is the only
//! way to tell them apart in the outline); qualified method extensions keep the
//! full name (`Base.show`).

use std::panic::AssertUnwindSafe;
use std::path::Path;

use lsp_types::{DocumentSymbol, Range, SymbolKind};

use crate::incremental::Analysis;
use crate::parser::parse;
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use crate::text::{LineIndex, PositionEncoding};

/// The document-symbol outline for `text`, re-parsing it. Pure and
/// unit-testable; single-file, so it never consults the workspace.
///
/// Best-effort, with no clean-parse gate: an outline of partial input is still
/// useful, and a definition whose name cannot be recovered contributes no
/// symbol of its own but its body is still walked.
pub fn compute_document_symbols(text: &str, encoding: PositionEncoding) -> Vec<DocumentSymbol> {
    let root = parse(text).cst;
    symbols_for_tree(&root, text, encoding)
}

/// Compute document symbols off the snapshot's cached parse when the db's
/// tracked buffer for `path` still matches `text`; otherwise re-parse. A write
/// racing the read trips `salsa::Cancelled`, which also falls back to a fresh
/// parse. Mirrors [`format_edits_via_db`](super::format::format_edits_via_db).
pub(crate) fn document_symbols_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &str,
    encoding: PositionEncoding,
) -> Vec<DocumentSymbol> {
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            // The tracked input lags the live buffer; the cached tree is stale.
            return None;
        }
        let root = snapshot.parsed_tree(file);
        Some(symbols_for_tree(&root, text, encoding))
    }));
    match cached {
        Ok(Some(symbols)) => symbols,
        // Cache miss (`Ok(None)`) or a racing write (`Err`): re-parse from text.
        Ok(None) | Err(_) => compute_document_symbols(text, encoding),
    }
}

/// Shared entry point for the fresh-parse and cached-tree paths: `root` must be
/// the parse tree of exactly `text`.
fn symbols_for_tree(
    root: &SyntaxNode,
    text: &str,
    encoding: PositionEncoding,
) -> Vec<DocumentSymbol> {
    let ctx = Ctx {
        text,
        line_index: LineIndex::new(text),
        encoding,
    };
    let mut out = Vec::new();
    collect_symbols(root, &ctx, &mut out);
    out
}

struct Ctx<'a> {
    text: &'a str,
    line_index: LineIndex<'a>,
    encoding: PositionEncoding,
}

impl Ctx<'_> {
    fn lsp_range(&self, range: rowan::TextRange) -> Range {
        Range::new(
            self.line_index
                .byte_to_position(range.start().into(), self.encoding),
            self.line_index
                .byte_to_position(range.end().into(), self.encoding),
        )
    }
}

/// Walk `node`'s child nodes, emitting symbols for definitions and descending
/// through every other node — that transparency is what lets a definition
/// nested in an `if`/`let`/`begin` (none of which introduce a symbol of their
/// own) surface at the right level instead of being dropped.
fn collect_symbols(node: &SyntaxNode, ctx: &Ctx<'_>, out: &mut Vec<DocumentSymbol>) {
    for child in node.children() {
        visit(&child, ctx, out);
    }
}

/// Emit the symbol(s) for `node` if it is a definition, else recurse into it.
fn visit(node: &SyntaxNode, ctx: &Ctx<'_>, out: &mut Vec<DocumentSymbol>) {
    let symbol = match node.kind() {
        SyntaxKind::MODULE_DEF
        | SyntaxKind::FUNCTION_DEF
        | SyntaxKind::MACRO_DEF
        | SyntaxKind::STRUCT_DEF
        | SyntaxKind::ABSTRACT_DEF
        | SyntaxKind::PRIMITIVE_DEF => def_symbol(node, ctx),
        SyntaxKind::CONST_STMT => {
            if const_symbols(node, ctx, out) {
                return;
            }
            None
        }
        SyntaxKind::ASSIGNMENT_EXPR => short_form_symbol(node, ctx),
        _ => None,
    };
    match symbol {
        Some(symbol) => out.push(symbol),
        // Not a definition (or its name is unrecoverable): descend so nested
        // definitions still surface.
        None => collect_symbols(node, ctx, out),
    }
}

/// The symbol for a keyword-introduced definition (`module`, `function`,
/// `macro`, `struct`, `abstract type`, `primitive type`), or `None` when the
/// defined name cannot be recovered (interpolated, anonymous, or parse error).
fn def_symbol(def: &SyntaxNode, ctx: &Ctx<'_>) -> Option<DocumentSymbol> {
    let sig_expr = signature_expr(def)?;
    let head = unwrap_head(sig_expr.clone());

    let function_like = matches!(def.kind(), SyntaxKind::FUNCTION_DEF | SyntaxKind::MACRO_DEF);
    let (mut name, selection) = if function_like && head.kind() == SyntaxKind::CALL_EXPR {
        callee_name(&head)?
    } else {
        head_name(&head)?
    };
    if def.kind() == SyntaxKind::MACRO_DEF {
        name.insert(0, '@');
    }
    let detail = function_like
        .then(|| signature_detail(&sig_expr, selection, ctx.text))
        .flatten();

    let kind = match def.kind() {
        SyntaxKind::MODULE_DEF => SymbolKind::MODULE,
        SyntaxKind::STRUCT_DEF | SyntaxKind::PRIMITIVE_DEF => SymbolKind::STRUCT,
        SyntaxKind::ABSTRACT_DEF => SymbolKind::INTERFACE,
        _ => SymbolKind::FUNCTION,
    };

    let mut children = Vec::new();
    if let Some(block) = child_of_kind(def, SyntaxKind::BLOCK) {
        if def.kind() == SyntaxKind::STRUCT_DEF {
            collect_struct_members(&block, ctx, &mut children);
        } else {
            collect_symbols(&block, ctx, &mut children);
        }
    }

    Some(make_symbol(
        name,
        detail,
        kind,
        def.text_range(),
        selection,
        children,
        ctx,
    ))
}

/// The symbol for a short-form function definition (`f(x) = ...`), or `None`
/// for any other assignment. The LHS must be a call (possibly under `where` or
/// `::` wrappers) and the operator a plain `=`.
fn short_form_symbol(assign: &SyntaxNode, ctx: &Ctx<'_>) -> Option<DocumentSymbol> {
    if op_token(assign)?.kind() != SyntaxKind::EQ {
        return None;
    }
    let lhs = assign.children().next()?;
    let head = unwrap_head(lhs.clone());
    if head.kind() != SyntaxKind::CALL_EXPR {
        return None;
    }
    let (name, selection) = callee_name(&head)?;
    let detail = signature_detail(&lhs, selection, ctx.text);

    // Nested definitions live in the value side; the signature binds none.
    let mut children = Vec::new();
    for rhs in assign.children().skip(1) {
        visit(&rhs, ctx, &mut children);
    }

    Some(make_symbol(
        name,
        detail,
        SymbolKind::FUNCTION,
        assign.text_range(),
        selection,
        children,
        ctx,
    ))
}

/// Emit one `CONSTANT` per name declared by a `const` statement (`const x = 1`,
/// `const a, b = 1, 2`, `const x::Int`, `const global x`). Returns whether any
/// symbol was emitted.
fn const_symbols(stmt: &SyntaxNode, ctx: &Ctx<'_>, out: &mut Vec<DocumentSymbol>) -> bool {
    // `const global x` nests the declaration in a GLOBAL_STMT.
    let inner = match stmt.children().next() {
        Some(node) if node.kind() == SyntaxKind::GLOBAL_STMT => node.children().next(),
        node => node,
    };
    let Some(inner) = inner else {
        return false;
    };
    // `const x = 1` declares the LHS; `const x` / `const x::Int` are bare.
    let target = if inner.kind() == SyntaxKind::ASSIGNMENT_EXPR {
        match inner.children().next() {
            Some(lhs) => lhs,
            None => return false,
        }
    } else {
        inner
    };
    let names: Vec<_> = if target.kind() == SyntaxKind::BARE_TUPLE_EXPR {
        target
            .children()
            .filter_map(|item| head_name(&unwrap_head(item)))
            .collect()
    } else {
        head_name(&unwrap_head(target)).into_iter().collect()
    };
    let emitted = !names.is_empty();
    for (name, selection) in names {
        out.push(make_symbol(
            name,
            None,
            SymbolKind::CONSTANT,
            stmt.text_range(),
            selection,
            Vec::new(),
            ctx,
        ));
    }
    emitted
}

/// Walk a struct body: bare names and `x::T` annotations (optionally under a
/// `const` field marker or a `@kwdef`-style `= default`) become `FIELD`
/// symbols; inner constructors and any other nested definitions go through the
/// normal definition walk.
fn collect_struct_members(block: &SyntaxNode, ctx: &Ctx<'_>, out: &mut Vec<DocumentSymbol>) {
    for child in block.children() {
        match child.kind() {
            SyntaxKind::NAME | SyntaxKind::TYPE_ANNOTATION => {
                field_symbol(&child, &child, ctx, out);
            }
            SyntaxKind::CONST_STMT => {
                if let Some(inner) = child.children().next() {
                    field_symbol(&child, &inner, ctx, out);
                }
            }
            SyntaxKind::ASSIGNMENT_EXPR => {
                // An inner constructor's short form, or a `@kwdef` default.
                if let Some(symbol) = short_form_symbol(&child, ctx) {
                    out.push(symbol);
                } else if let Some(lhs) = child.children().next() {
                    field_symbol(&child, &lhs, ctx, out);
                }
            }
            _ => visit(&child, ctx, out),
        }
    }
}

/// Emit the `FIELD` symbol for a struct member whose declaration spans `full`
/// and whose name lives in `target` (a NAME or `x::T` annotation). Anything
/// else — e.g. a stray expression statement — emits nothing.
fn field_symbol(
    full: &SyntaxNode,
    target: &SyntaxNode,
    ctx: &Ctx<'_>,
    out: &mut Vec<DocumentSymbol>,
) {
    if !matches!(
        target.kind(),
        SyntaxKind::NAME | SyntaxKind::TYPE_ANNOTATION | SyntaxKind::NONSTANDARD_IDENTIFIER
    ) {
        return;
    }
    if let Some((name, selection)) = head_name(&unwrap_head(target.clone())) {
        out.push(make_symbol(
            name,
            None,
            SymbolKind::FIELD,
            full.text_range(),
            selection,
            Vec::new(),
            ctx,
        ));
    }
}

/// The expression inside a definition's `SIGNATURE` child.
pub(crate) fn signature_expr(def: &SyntaxNode) -> Option<SyntaxNode> {
    child_of_kind(def, SyntaxKind::SIGNATURE)?.children().next()
}

/// Strip `where` clauses and `::` return-type annotations down to the callable
/// or named head: `f(x)::T where U` → `f(x)`, `x::Int` → `x`.
pub(crate) fn unwrap_head(mut node: SyntaxNode) -> SyntaxNode {
    while matches!(
        node.kind(),
        SyntaxKind::WHERE_EXPR | SyntaxKind::TYPE_ANNOTATION
    ) {
        match node.first_child() {
            Some(inner) => node = inner,
            None => break,
        }
    }
    node
}

/// The defined name and its selection range for a call signature's callee: a
/// plain or `var"..."` name, a dot-qualified path (`Base.show`, `Base.:+`), a
/// parametric constructor head (`Foo{T}`), or a bare operator (`+`).
pub(crate) fn callee_name(call: &SyntaxNode) -> Option<(String, rowan::TextRange)> {
    let callee = call
        .children_with_tokens()
        .find(|el| !is_trivia(el.kind()))?;
    match callee {
        rowan::NodeOrToken::Node(node) => head_name(&node),
        rowan::NodeOrToken::Token(token) => Some((token.text().to_string(), token.text_range())),
    }
}

/// The display name and selection range of a name-position expression, or
/// `None` when it has no static name (interpolation, tuple, error node).
pub(crate) fn head_name(node: &SyntaxNode) -> Option<(String, rowan::TextRange)> {
    match node.kind() {
        SyntaxKind::NAME => {
            let ident = node
                .children_with_tokens()
                .filter_map(|el| el.into_token())
                .find(|t| !is_trivia(t.kind()))?;
            Some((ident.text().to_string(), node.text_range()))
        }
        // `var"weird name"`: the display name is the quoted content.
        SyntaxKind::NONSTANDARD_IDENTIFIER => {
            let content = node
                .children_with_tokens()
                .filter_map(|el| el.into_token())
                .find(|t| t.kind() == SyntaxKind::STRING_CONTENT)?;
            Some((content.text().to_string(), node.text_range()))
        }
        SyntaxKind::BINARY_EXPR => {
            let op = op_token(node)?;
            if op.kind() == SyntaxKind::DOT {
                // A qualified path: render it whole, minus any interior trivia.
                let text: String = node
                    .descendants_with_tokens()
                    .filter_map(|el| el.into_token())
                    .filter(|t| !is_trivia(t.kind()))
                    .map(|t| t.text().to_string())
                    .collect();
                Some((text, node.text_range()))
            } else {
                // `A <: B` and friends: the name is the left operand.
                head_name(&node.first_child()?)
            }
        }
        // `Foo{T}`: the base name, with the parameters left to `detail`.
        SyntaxKind::CURLY_EXPR => head_name(&node.first_child()?),
        _ => None,
    }
}

/// The signature rendered after the name — `(x::Int, y) where T` for
/// `f(x::Int, y) where T` — as the symbol's `detail`. `None` when nothing
/// follows the name (`function f end`).
pub(crate) fn signature_detail(
    sig_expr: &SyntaxNode,
    name_range: rowan::TextRange,
    text: &str,
) -> Option<String> {
    let start = usize::from(name_range.end());
    let end = usize::from(sig_expr.text_range().end());
    let detail = text.get(start..end)?.trim();
    (!detail.is_empty()).then(|| detail.to_string())
}

/// The first non-trivia operator-position token of a binary-shaped node (the
/// token after its first child node).
pub(crate) fn op_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    let first_child_end = node.first_child()?.text_range().end();
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.text_range().start() >= first_child_end && !is_trivia(t.kind()))
}

fn child_of_kind(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxNode> {
    node.children().find(|c| c.kind() == kind)
}

pub(crate) fn is_trivia(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::WHITESPACE
            | SyntaxKind::NEWLINE
            | SyntaxKind::COMMENT
            | SyntaxKind::BLOCK_COMMENT
    )
}

#[expect(deprecated, reason = "DocumentSymbol::deprecated is a required field")]
fn make_symbol(
    name: String,
    detail: Option<String>,
    kind: SymbolKind,
    range: rowan::TextRange,
    selection: rowan::TextRange,
    children: Vec<DocumentSymbol>,
    ctx: &Ctx<'_>,
) -> DocumentSymbol {
    DocumentSymbol {
        name,
        detail,
        kind,
        tags: None,
        deprecated: None,
        range: ctx.lsp_range(range),
        selection_range: ctx.lsp_range(selection),
        children: (!children.is_empty()).then_some(children),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incremental::IncrementalDatabase;

    /// The cached-tree path matches the re-parse path when the db's tracked
    /// buffer is the live text, and falls back (still correctly) when the db
    /// lags the buffer or has never seen the path.
    #[test]
    fn symbols_via_db_match_compute_and_fall_back() {
        let encoding = PositionEncoding::Utf16;
        let path = Path::new("/work/a.jl");
        let buffer = "f(x) = x\n";
        let expected = compute_document_symbols(buffer, encoding);
        assert_eq!(expected.len(), 1, "fixture must yield a symbol");

        // Cache hit: tracked text == buffer → symbols off the cached tree.
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, buffer.to_string());
        assert_eq!(
            document_symbols_via_db(&db.snapshot(), path, buffer, encoding),
            expected,
            "cached-tree symbols must match the re-parse path"
        );

        // Stale db (tracked text lags the buffer) → fall back to a fresh parse.
        let mut stale = IncrementalDatabase::default();
        stale.upsert_file(path, "y = 1\n".to_string());
        assert_eq!(
            document_symbols_via_db(&stale.snapshot(), path, buffer, encoding),
            expected,
            "version skew must fall back to the buffer text"
        );

        // Untracked path → fall back as well.
        let empty = IncrementalDatabase::default();
        assert_eq!(
            document_symbols_via_db(&empty.snapshot(), path, buffer, encoding),
            expected,
            "untracked path must fall back to the buffer text"
        );
    }
}
