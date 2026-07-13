//! Semantic tokens (`textDocument/semanticTokens/full`): syntax-driven
//! highlighting from a pure CST walk — keywords, macro names, string-macro
//! prefixes/suffixes, and literals — refined with resolved names. Identifiers
//! classify as function, type, or module (`namespace` on the wire) by what
//! they resolve to: a file binding's [`BindingKind`], or, for free reads, the
//! shared masking order ([`Resolver::resolve`]) followed by a kind lookup in
//! the harvested library. Qualified reads (`Base.Threads.@spawn`) paint each
//! resolvable module component as a namespace and the final member by its
//! library kind.
//!
//! Conventions: a macro name paints as one token over the sigil and the final
//! name component (`@show`; `@time` in `Base.@time`), string delimiters and
//! content coalesce into one string token per line, string-macro prefixes and
//! suffixes paint as macros (`r"x"` calls `@r_str`; the suffix is an argument
//! to it), and interpolations inside strings stay unpainted so they render as
//! code. Names that resolve to nothing paintable — locals, parameters,
//! consts, operators (`x + y` lighting up as "function" is noise), and
//! unresolved reads — stay plain. Tokens never span line breaks — most
//! clients reject multiline semantic tokens — so multi-line spans are split
//! per line before encoding.
//!
//! Deferred: `import`ed names are not chased into the library (matching hover
//! and go-to-definition), and `using`/`import` statement paths stay plain.

use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::Arc;

use lsp_types::{Position, SemanticToken, SemanticTokenType, SemanticTokens, SemanticTokensLegend};
use rowan::{TextRange, TextSize};
use smol_str::SmolStr;

use crate::incremental::Analysis;
use crate::index::{ModuleIndex, PackageIndex};
use crate::parser::parse;
use crate::resolve::{
    ModulePath, Namespace, PackageSource, Resolution, Resolver, module_at, resolve_submodule,
};
use crate::semantic::{BindingKind, LoadKind, SemanticModel};
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use crate::text::{LineIndex, PositionEncoding};

/// The token classes this server emits; the discriminant is the index into
/// [`legend`]'s `token_types`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HighlightKind {
    Keyword = 0,
    Macro = 1,
    String = 2,
    Number = 3,
    Function = 4,
    Type = 5,
    /// A module name; the LSP legend calls this `namespace`.
    Module = 6,
}

/// The legend advertised in the server capabilities. Order must match
/// [`HighlightKind`]'s discriminants.
pub(crate) fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::KEYWORD,
            SemanticTokenType::MACRO,
            SemanticTokenType::STRING,
            SemanticTokenType::NUMBER,
            SemanticTokenType::FUNCTION,
            SemanticTokenType::TYPE,
            SemanticTokenType::NAMESPACE,
        ],
        token_modifiers: vec![],
    }
}

/// The semantic tokens for `text`, re-parsing it. Pure and unit-testable;
/// `packages` supplies the library (Base/Core and loaded packages) for
/// classifying free reads.
///
/// Best-effort, with no clean-parse gate: highlighting the intact parts of a
/// broken buffer is still useful while the user types.
pub fn compute_semantic_tokens<P: PackageSource>(
    text: &str,
    encoding: PositionEncoding,
    packages: &P,
) -> SemanticTokens {
    let root = parse(text).cst;
    let model = SemanticModel::build(&root);
    // The pure path has no file path to key workspace membership on; the live
    // server passes the workspace module through `semantic_tokens_via_db`.
    tokens_for(&root, &model, packages, None, text, encoding)
}

/// Compute semantic tokens off the snapshot's cached parse when the db's
/// tracked buffer for `path` still matches `text`; otherwise re-parse. A write
/// racing the read trips `salsa::Cancelled`, which also falls back to a fresh
/// parse. Mirrors [`selection_ranges_via_db`](super::selection::selection_ranges_via_db).
pub(crate) fn semantic_tokens_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &str,
    encoding: PositionEncoding,
) -> SemanticTokens {
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            // The tracked input lags the live buffer; the cached tree is stale.
            return None;
        }
        let root = snapshot.parsed_tree(file);
        let model = snapshot.semantic_model(file);
        let workspace = snapshot.workspace_member(path);
        Some(tokens_for(
            &root, model, snapshot, workspace, text, encoding,
        ))
    }));
    match cached {
        Ok(Some(tokens)) => tokens,
        // Cache miss (`Ok(None)`) or a racing write (`Err`): re-parse from text.
        Ok(None) | Err(_) => compute_semantic_tokens(text, encoding, snapshot),
    }
}

/// Shared entry point for the fresh-parse and cached-tree paths: `root` and
/// `model` must both belong to exactly `text`. Syntax-driven spans (keywords,
/// macros, literals) and resolved-name spans (identifiers classified by what
/// they resolve to) merge into one position-ordered stream; on an overlap the
/// syntax span wins (a name painted both ways carries the same kind anyway).
fn tokens_for<P: PackageSource + ?Sized>(
    root: &SyntaxNode,
    model: &SemanticModel,
    packages: &P,
    workspace: Option<(Arc<PackageIndex>, ModulePath)>,
    text: &str,
    encoding: PositionEncoding,
) -> SemanticTokens {
    let mut spans = syntax_spans(root);
    spans.extend(resolved_spans(model, packages, workspace, text));
    // Stable, so a resolved span sharing a syntax span's start sorts after it
    // and the overlap drop keeps the syntax paint.
    spans.sort_by_key(|&(range, _)| (range.start(), range.end()));
    let spans = drop_overlaps(spans);
    SemanticTokens {
        result_id: None,
        data: delta_encode(&spans, text, encoding),
    }
}

/// The syntax-driven spans of the tree, in document order: keywords, macro
/// names, string parts, and literals.
fn syntax_spans(root: &SyntaxNode) -> Vec<(TextRange, HighlightKind)> {
    let mut spans: Vec<(TextRange, HighlightKind)> = Vec::new();
    for token in root
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
    {
        let Some(kind) = classify(&token) else {
            continue;
        };
        match spans.last_mut() {
            // Coalesce byte-adjacent same-kind spans: `@` + name become one
            // macro token, delimiters + content one string token.
            Some((range, last)) if *last == kind && range.end() == token.text_range().start() => {
                *range = TextRange::new(range.start(), token.text_range().end());
            }
            _ => spans.push((token.text_range(), kind)),
        }
    }
    spans
}

/// Keep the first of any overlapping spans in a position-sorted list.
/// Duplicates arise when a name paints both syntactically and through
/// resolution (e.g. the macro member of a qualified read).
fn drop_overlaps(spans: Vec<(TextRange, HighlightKind)>) -> Vec<(TextRange, HighlightKind)> {
    let mut out: Vec<(TextRange, HighlightKind)> = Vec::with_capacity(spans.len());
    for (range, kind) in spans {
        match out.last() {
            Some((prev, _)) if range.start() < prev.end() => {}
            _ => out.push((range, kind)),
        }
    }
    out
}

/// The resolved-name spans of the file, unordered: definition-site names,
/// export-list names, resolved identifier occurrences, free reads classified
/// through the shared masking order, and qualified-read components.
fn resolved_spans<P: PackageSource + ?Sized>(
    model: &SemanticModel,
    packages: &P,
    workspace: Option<(Arc<PackageIndex>, ModulePath)>,
    text: &str,
) -> Vec<(TextRange, HighlightKind)> {
    let mut spans = Vec::new();
    // Definition sites: the name in `function f`, `macro m`, `struct S`,
    // `module M`. Bindings are not occurrences, so they need their own pass.
    for binding in model.bindings() {
        if !is_identifier_shaped(&binding.name) {
            continue;
        }
        if let Some(kind) = classify_binding(binding.kind) {
            spans.push((binding.def_range, kind));
        }
    }
    // Export-list names resolve to their global-scope binding.
    for entry in model.exports() {
        if let Some(bid) = entry.binding
            && let Some(kind) = classify_binding(model.binding(bid).kind)
        {
            spans.push((entry.range, kind));
        }
    }
    let resolver = Resolver::new(model, packages).with_workspace(workspace.clone());
    for ident in model.idents() {
        // Macro reads already paint syntactically; operators stay plain.
        if ident.is_macro || !is_identifier_shaped(&ident.name) {
            continue;
        }
        let kind = match ident.binding {
            Some(bid) => classify_binding(model.binding(bid).kind),
            None => classify_free_read(
                model,
                packages,
                &workspace,
                &resolver,
                &ident.name,
                ident.range.start(),
            ),
        };
        if let Some(kind) = kind {
            spans.push((ident.range, kind));
        }
    }
    spans.extend(qualified_spans(model, packages, text));
    spans
}

/// The highlight class a binding's kind maps to; `None` for the kinds the
/// legend does not (yet) cover — locals, parameters, consts, imports.
fn classify_binding(kind: BindingKind) -> Option<HighlightKind> {
    match kind {
        BindingKind::Function => Some(HighlightKind::Function),
        BindingKind::Macro => Some(HighlightKind::Macro),
        BindingKind::Type => Some(HighlightKind::Type),
        BindingKind::Module => Some(HighlightKind::Module),
        _ => None,
    }
}

/// Whether `name` is a plain identifier rather than an operator: operator
/// reads resolving to library functions stay plain — most themes paint
/// operators distinctly, and `x + y` lighting up as "function" is noise.
fn is_identifier_shaped(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
}

/// Classify a free (non-local, non-qualified) read by resolving it through
/// the shared masking order and looking its kind up in the target module.
fn classify_free_read<P: PackageSource + ?Sized>(
    model: &SemanticModel,
    packages: &P,
    workspace: &Option<(Arc<PackageIndex>, ModulePath)>,
    resolver: &Resolver<'_, P>,
    name: &str,
    offset: TextSize,
) -> Option<HighlightKind> {
    match resolver.resolve(name, offset, Namespace::Value) {
        // A binding the occurrence walk missed but resolution finds: still local.
        Resolution::Binding(bid) => classify_binding(model.binding(bid).kind),
        // A same-module sibling: look the name up in the file's host module.
        Resolution::Workspace { module, name } => {
            let (pkg, _) = workspace.as_ref()?;
            library_kind(module_at(&pkg.root, &module)?, &name)
        }
        Resolution::System { module, name } => {
            library_kind(&packages.package(&module)?.root, &name)
        }
        Resolution::Using { module, name } => using_kind(model, packages, &module, &name),
        Resolution::Unresolved => None,
    }
}

/// Find the module a whole-module `using` brings in and classify `name` from
/// it. `module` is the clause's display name (its last component): a plain
/// `using LinearAlgebra` names the package directly; a `using A.B` needs the
/// clause walked from its package root. Mirrors hover's `library_from_using`.
fn using_kind<P: PackageSource + ?Sized>(
    model: &SemanticModel,
    packages: &P,
    module: &str,
    name: &str,
) -> Option<HighlightKind> {
    if let Some(pkg) = packages.package(module)
        && let Some(kind) = library_kind(&pkg.root, name)
    {
        return Some(kind);
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
            && let Some(kind) = library_kind(m, name)
        {
            return Some(kind);
        }
    }
    None
}

/// Look `name` up among `module`'s defined symbols and map it to a highlight
/// class. Mirrors the search order of hover's `render_library_symbol`, plus
/// submodules; consts stay plain — the legend has no constant type (yet).
fn library_kind(module: &ModuleIndex, name: &str) -> Option<HighlightKind> {
    if name.starts_with('@') {
        return module
            .macros
            .iter()
            .any(|m| m.name == name)
            .then_some(HighlightKind::Macro);
    }
    if module.functions.iter().any(|f| f.name == name) {
        return Some(HighlightKind::Function);
    }
    if module.types.iter().any(|t| t.name == name) {
        return Some(HighlightKind::Type);
    }
    if module.submodules.iter().any(|m| m.name == name) {
        return Some(HighlightKind::Module);
    }
    None
}

/// The spans of qualified reads (`Base.Threads.@spawn`): each module
/// component that resolves against the library paints as a namespace, and the
/// final member by its kind in the resolved module (a macro member is already
/// painted syntactically, so it is skipped here and deduped by the overlap
/// drop if it were not).
fn qualified_spans<P: PackageSource + ?Sized>(
    model: &SemanticModel,
    packages: &P,
    text: &str,
) -> Vec<(TextRange, HighlightKind)> {
    let mut spans = Vec::new();
    for q in model.qualified_reads() {
        let Some((member, modules)) = q.path.split_last() else {
            continue;
        };
        let Some(head) = modules.first() else {
            continue;
        };
        let Some(pkg) = packages.package(head) else {
            continue;
        };
        let Some(ranges) = component_ranges(text, q.range, &q.path) else {
            continue;
        };
        spans.push((ranges[0], HighlightKind::Module));
        let mut module = Some(&pkg.root);
        for (i, comp) in modules.iter().enumerate().skip(1) {
            module = module.and_then(|m| m.submodules.iter().find(|s| s.name == comp.as_str()));
            match module {
                Some(_) => spans.push((ranges[i], HighlightKind::Module)),
                None => break,
            }
        }
        if let Some(m) = module
            && !q.is_macro
            && let Some(kind) = library_kind(m, member)
        {
            spans.push((ranges[modules.len()], kind));
        }
    }
    spans
}

/// The range of each dotted component inside `range`, found by scanning the
/// chain's text left to right (components appear in order, so a plain
/// substring search cannot land on a later component); `None` when any
/// component cannot be located.
fn component_ranges(text: &str, range: TextRange, path: &[SmolStr]) -> Option<Vec<TextRange>> {
    let base = usize::from(range.start());
    let slice = &text[base..usize::from(range.end())];
    let mut ranges = Vec::with_capacity(path.len());
    let mut cursor = 0;
    for comp in path {
        let at = slice[cursor..].find(comp.as_str())? + cursor;
        let start = (base + at) as u32;
        ranges.push(TextRange::new(
            TextSize::new(start),
            TextSize::new(start + comp.len() as u32),
        ));
        cursor = at + comp.len();
    }
    Some(ranges)
}

/// The highlight class for a single token, if any. Structural rules (macro
/// names, string parts) go by the parent node; everything else by kind alone.
fn classify(token: &SyntaxToken) -> Option<HighlightKind> {
    match token.parent().map(|parent| parent.kind()) {
        Some(SyntaxKind::MACRO_NAME) => classify_in_macro_name(token),
        Some(SyntaxKind::STRING_LITERAL | SyntaxKind::CMD_LITERAL) => {
            classify_in_string(token.kind())
        }
        // `var"..."` bodies (NONSTANDARD_IDENTIFIER) fall through here and
        // stay plain: they are identifiers, not strings.
        _ => classify_by_kind(token.kind()),
    }
}

/// Inside a `MACRO_NAME`, paint the sigil and the final name component — the
/// `x` in `@A.B.x`, the operator in `@+`, the keyword in `@macro` — leaving
/// qualifiers to the resolved-name pass ([`qualified_spans`] paints the ones
/// the library resolves). Trailing-sigil qualifiers (`A.B.@x`) sit in nested
/// nodes, so the parent gate already excludes them; only the leading-sigil
/// path components need skipping here.
fn classify_in_macro_name(token: &SyntaxToken) -> Option<HighlightKind> {
    if token.kind() == SyntaxKind::AT {
        return Some(HighlightKind::Macro);
    }
    // The `)` closing the parenthesized `@(expr)` form names nothing.
    if token.kind() != SyntaxKind::RPAREN && is_last_name_token(token) {
        return Some(HighlightKind::Macro);
    }
    None
}

/// Whether `token` is the last non-trivia token directly under its parent.
fn is_last_name_token(token: &SyntaxToken) -> bool {
    let Some(parent) = token.parent() else {
        return false;
    };
    parent
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !is_trivia(t.kind()))
        .last()
        .is_some_and(|last| last == *token)
}

/// Inside a string or command literal: delimiters and content are string;
/// the macro prefix and suffix flags are macros. A numeric string-macro
/// suffix (`x"1"2`) falls through to the kind rules and paints as a number.
fn classify_in_string(kind: SyntaxKind) -> Option<HighlightKind> {
    match kind {
        SyntaxKind::STRING_CONTENT
        | SyntaxKind::STRING_DELIM_OPEN
        | SyntaxKind::STRING_DELIM_CLOSE
        | SyntaxKind::CMD_DELIM_OPEN
        | SyntaxKind::CMD_DELIM_CLOSE => Some(HighlightKind::String),
        SyntaxKind::STRING_PREFIX | SyntaxKind::STRING_SUFFIX => Some(HighlightKind::Macro),
        _ => classify_by_kind(kind),
    }
}

/// Context-free classification: keywords and non-string literals.
fn classify_by_kind(kind: SyntaxKind) -> Option<HighlightKind> {
    if is_keyword(kind) {
        return Some(HighlightKind::Keyword);
    }
    match kind {
        SyntaxKind::CHAR => Some(HighlightKind::String),
        SyntaxKind::INTEGER
        | SyntaxKind::BIN_INT
        | SyntaxKind::OCT_INT
        | SyntaxKind::HEX_INT
        | SyntaxKind::FLOAT
        | SyntaxKind::FLOAT32 => Some(HighlightKind::Number),
        _ => None,
    }
}

/// All keyword tokens, `true`/`false` included: the standard legend has no
/// boolean type, and `keyword` matches the lexer's classification.
fn is_keyword(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::FUNCTION_KW
            | SyntaxKind::MACRO_KW
            | SyntaxKind::END_KW
            | SyntaxKind::IF_KW
            | SyntaxKind::ELSEIF_KW
            | SyntaxKind::ELSE_KW
            | SyntaxKind::BEGIN_KW
            | SyntaxKind::TRUE_KW
            | SyntaxKind::FALSE_KW
            | SyntaxKind::WHILE_KW
            | SyntaxKind::FOR_KW
            | SyntaxKind::LET_KW
            | SyntaxKind::QUOTE_KW
            | SyntaxKind::TRY_KW
            | SyntaxKind::CATCH_KW
            | SyntaxKind::FINALLY_KW
            | SyntaxKind::STRUCT_KW
            | SyntaxKind::MUTABLE_KW
            | SyntaxKind::MODULE_KW
            | SyntaxKind::BAREMODULE_KW
            | SyntaxKind::DO_KW
            | SyntaxKind::RETURN_KW
            | SyntaxKind::BREAK_KW
            | SyntaxKind::CONTINUE_KW
            | SyntaxKind::CONST_KW
            | SyntaxKind::GLOBAL_KW
            | SyntaxKind::LOCAL_KW
            | SyntaxKind::IMPORT_KW
            | SyntaxKind::USING_KW
            | SyntaxKind::EXPORT_KW
            | SyntaxKind::WHERE_KW
    )
}

fn is_trivia(kind: SyntaxKind) -> bool {
    matches!(kind, SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE)
}

/// Fold position-ordered spans into the LSP's relative encoding, splitting
/// multi-line spans first. `delta_start` and `length` count code units of the
/// negotiated encoding, which [`LineIndex::byte_to_position`] produces.
fn delta_encode(
    spans: &[(TextRange, HighlightKind)],
    text: &str,
    encoding: PositionEncoding,
) -> Vec<SemanticToken> {
    let line_index = LineIndex::new(text);
    let mut data = Vec::new();
    let mut prev = Position::new(0, 0);
    for &(range, kind) in spans {
        for segment in split_at_line_breaks(range, text) {
            let start = line_index.byte_to_position(segment.start().into(), encoding);
            let end = line_index.byte_to_position(segment.end().into(), encoding);
            debug_assert_eq!(start.line, end.line, "segments never span line breaks");
            let delta_line = start.line - prev.line;
            let delta_start = if delta_line == 0 {
                start.character - prev.character
            } else {
                start.character
            };
            data.push(SemanticToken {
                delta_line,
                delta_start,
                length: end.character - start.character,
                token_type: kind as u32,
                token_modifiers_bitset: 0,
            });
            prev = start;
        }
    }
    data
}

/// Split `range` at line breaks into per-line, non-empty segments, excluding
/// the `\n` (and a preceding `\r`) itself.
fn split_at_line_breaks(range: TextRange, text: &str) -> Vec<TextRange> {
    let base = usize::from(range.start());
    let slice = &text[base..usize::from(range.end())];
    let mut segments = Vec::new();
    let mut push = |from: usize, to: usize| {
        let to = if slice.as_bytes()[from..to].last() == Some(&b'\r') {
            to - 1
        } else {
            to
        };
        if to > from {
            segments.push(TextRange::new(
                TextSize::new((base + from) as u32),
                TextSize::new((base + to) as u32),
            ));
        }
    };
    let mut start = 0;
    for (i, byte) in slice.bytes().enumerate() {
        if byte == b'\n' {
            push(start, i);
            start = i + 1;
        }
    }
    push(start, slice.len());
    segments
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use lsp_types::Position;

    use crate::incremental::IncrementalDatabase;
    use crate::index::model::{DefLocation, ExportedName, Span, Visibility};
    use crate::index::{
        ConstDef, FunctionGroup, MacroDef, ModuleIndex, PackageIndex, TypeDef, TypeKind,
    };

    /// The legend's order is the contract between [`HighlightKind`]'s
    /// discriminants and the indices on the wire.
    #[test]
    fn legend_order_matches_highlight_kind_discriminants() {
        let legend = legend();
        for (kind, token_type) in [
            (HighlightKind::Keyword, SemanticTokenType::KEYWORD),
            (HighlightKind::Macro, SemanticTokenType::MACRO),
            (HighlightKind::String, SemanticTokenType::STRING),
            (HighlightKind::Number, SemanticTokenType::NUMBER),
            (HighlightKind::Function, SemanticTokenType::FUNCTION),
            (HighlightKind::Type, SemanticTokenType::TYPE),
            (HighlightKind::Module, SemanticTokenType::NAMESPACE),
        ] {
            assert_eq!(legend.token_types[kind as usize], token_type);
        }
        assert_eq!(legend.token_types.len(), 7, "every kind is in the legend");
        assert!(legend.token_modifiers.is_empty());
    }

    // --- library builders (mirroring `hover::tests`) -------------------------

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
            members: Vec::new(),
            member_modules: Default::default(),
            diagnostics: Vec::new(),
        })
    }

    fn library(pkgs: Vec<Arc<PackageIndex>>) -> BTreeMap<String, Arc<PackageIndex>> {
        pkgs.into_iter().map(|p| (p.name.clone(), p)).collect()
    }

    fn function(name: &str) -> FunctionGroup {
        FunctionGroup {
            name: name.to_string(),
            owner: None,
            methods: Vec::new(),
            doc: None,
        }
    }

    fn typedef(name: &str) -> TypeDef {
        TypeDef {
            name: name.to_string(),
            kind: TypeKind::Struct { mutable: false },
            type_params: Vec::new(),
            supertype: None,
            fields: Vec::new(),
            doc: None,
            loc: loc(),
        }
    }

    fn macrodef(name: &str) -> MacroDef {
        MacroDef {
            name: name.to_string(),
            params: Vec::new(),
            doc: None,
            loc: loc(),
        }
    }

    fn no_library() -> BTreeMap<String, Arc<PackageIndex>> {
        BTreeMap::new()
    }

    /// Decode the tokens for `src` into `(painted text, kind)` pairs, in
    /// document order.
    fn painted(src: &str, lib: &BTreeMap<String, Arc<PackageIndex>>) -> Vec<(String, u32)> {
        let tokens = compute_semantic_tokens(src, PositionEncoding::Utf8, lib);
        let line_index = LineIndex::new(src);
        let (mut line, mut character) = (0u32, 0u32);
        let mut out = Vec::new();
        for t in &tokens.data {
            if t.delta_line > 0 {
                line += t.delta_line;
                character = 0;
            }
            character += t.delta_start;
            let start =
                line_index.position_to_byte(Position::new(line, character), PositionEncoding::Utf8);
            out.push((
                src[start..start + t.length as usize].to_string(),
                t.token_type,
            ));
        }
        out
    }

    /// `expected` as owned pairs, for comparing against [`painted`].
    fn expect(pairs: &[(&str, HighlightKind)]) -> Vec<(String, u32)> {
        pairs
            .iter()
            .map(|&(text, kind)| (text.to_string(), kind as u32))
            .collect()
    }

    // --- resolved-name classification ----------------------------------------

    #[test]
    fn function_names_paint_at_definition_and_use() {
        assert_eq!(
            painted("square(x) = x * x\nsquare(2)\n", &no_library()),
            expect(&[
                ("square", HighlightKind::Function),
                ("square", HighlightKind::Function),
                ("2", HighlightKind::Number),
            ]),
        );
    }

    #[test]
    fn definition_names_paint_by_binding_kind() {
        let src = "module M\nfunction f(x)\n    x\nend\nmacro m(x)\n    x\nend\nstruct S\n    a\nend\nend\n";
        assert_eq!(
            painted(src, &no_library()),
            expect(&[
                ("module", HighlightKind::Keyword),
                ("M", HighlightKind::Module),
                ("function", HighlightKind::Keyword),
                ("f", HighlightKind::Function),
                ("end", HighlightKind::Keyword),
                ("macro", HighlightKind::Keyword),
                ("m", HighlightKind::Macro),
                ("end", HighlightKind::Keyword),
                ("struct", HighlightKind::Keyword),
                ("S", HighlightKind::Type),
                ("end", HighlightKind::Keyword),
                ("end", HighlightKind::Keyword),
            ]),
        );
    }

    #[test]
    fn local_uses_paint_by_their_binding_kind() {
        assert_eq!(
            painted("struct P\n    a\nend\nP\n", &no_library()),
            expect(&[
                ("struct", HighlightKind::Keyword),
                ("P", HighlightKind::Type),
                ("end", HighlightKind::Keyword),
                ("P", HighlightKind::Type),
            ]),
        );
        assert_eq!(
            painted("module M\nend\nM\n", &no_library()),
            expect(&[
                ("module", HighlightKind::Keyword),
                ("M", HighlightKind::Module),
                ("end", HighlightKind::Keyword),
                ("M", HighlightKind::Module),
            ]),
        );
    }

    #[test]
    fn variables_and_parameters_stay_plain() {
        assert_eq!(
            painted("x = 1\nfunction g(y)\n    x + y\nend\n", &no_library()),
            expect(&[
                ("1", HighlightKind::Number),
                ("function", HighlightKind::Keyword),
                ("g", HighlightKind::Function),
                ("end", HighlightKind::Keyword),
            ]),
        );
    }

    #[test]
    fn library_free_reads_classify_by_symbol_kind() {
        let mut base = module("Base", &["map", "Dict", "pi", "Threads"]);
        base.functions.push(function("map"));
        base.types.push(typedef("Dict"));
        base.consts.push(ConstDef {
            name: "pi".to_string(),
            value_repr: None,
            doc: None,
            loc: loc(),
        });
        base.submodules.push(module("Threads", &[]));
        let lib = library(vec![package(base)]);
        // `map` is a function, `Dict` a type, `Threads` a submodule; `pi` (a
        // const) stays plain — the legend has no constant type (yet). The
        // unresolved `xs` stays plain too.
        assert_eq!(
            painted("map(pi, xs)\nDict\nThreads\n", &lib),
            expect(&[
                ("map", HighlightKind::Function),
                ("Dict", HighlightKind::Type),
                ("Threads", HighlightKind::Module),
            ]),
        );
    }

    #[test]
    fn using_export_paints_from_the_used_module() {
        let mut a = module("A", &["greet"]);
        a.functions.push(function("greet"));
        let lib = library(vec![package(a)]);
        assert_eq!(
            painted("using A\ngreet(1)\n", &lib),
            expect(&[
                ("using", HighlightKind::Keyword),
                ("greet", HighlightKind::Function),
                ("1", HighlightKind::Number),
            ]),
        );
    }

    #[test]
    fn a_local_shadow_masks_the_library() {
        let mut base = module("Base", &["map"]);
        base.functions.push(function("map"));
        let lib = library(vec![package(base)]);
        // The parameter `map` shadows Base's function: both sites stay plain.
        assert_eq!(
            painted("function h(map)\n    map\nend\n", &lib),
            expect(&[
                ("function", HighlightKind::Keyword),
                ("h", HighlightKind::Function),
                ("end", HighlightKind::Keyword),
            ]),
        );
    }

    #[test]
    fn qualified_reads_paint_namespaces_and_the_member() {
        let mut inner = module("Threads", &[]);
        inner.macros.push(macrodef("@spawn"));
        inner.functions.push(function("nthreads"));
        let mut base = module("Base", &[]);
        base.submodules.push(inner);
        base.functions.push(function("map"));
        let lib = library(vec![package(base)]);
        assert_eq!(
            painted("Base.map(f, xs)\n", &lib),
            expect(&[
                ("Base", HighlightKind::Module),
                ("map", HighlightKind::Function),
            ]),
        );
        assert_eq!(
            painted("Base.Threads.nthreads()\n", &lib),
            expect(&[
                ("Base", HighlightKind::Module),
                ("Threads", HighlightKind::Module),
                ("nthreads", HighlightKind::Function),
            ]),
        );
        // The macro member paints syntactically; the qualifiers resolve.
        assert_eq!(
            painted("Base.Threads.@spawn f()\n", &lib),
            expect(&[
                ("Base", HighlightKind::Module),
                ("Threads", HighlightKind::Module),
                ("@spawn", HighlightKind::Macro),
            ]),
        );
        // An unharvested package leaves the whole chain plain.
        assert_eq!(
            painted("Nope.f(1)\n", &lib),
            expect(&[("1", HighlightKind::Number)]),
        );
    }

    #[test]
    fn operator_reads_stay_plain_even_when_they_resolve() {
        let mut base = module("Base", &["+"]);
        base.functions.push(function("+"));
        let lib = library(vec![package(base)]);
        assert_eq!(
            painted("1 + 2\n", &lib),
            expect(&[("1", HighlightKind::Number), ("2", HighlightKind::Number),]),
        );
    }

    #[test]
    fn export_list_names_paint_by_their_binding_kind() {
        assert_eq!(
            painted("module M\nexport f\nf(x) = x\nend\n", &no_library()),
            expect(&[
                ("module", HighlightKind::Keyword),
                ("M", HighlightKind::Module),
                ("export", HighlightKind::Keyword),
                ("f", HighlightKind::Function),
                ("f", HighlightKind::Function),
                ("end", HighlightKind::Keyword),
            ]),
        );
    }

    /// The cached-tree path matches the re-parse path when the db's tracked
    /// buffer is the live text, and falls back (still correctly) when the db
    /// lags the buffer or has never seen the path.
    #[test]
    fn semantic_tokens_via_db_match_compute_and_fall_back() {
        let path = Path::new("/work/a.jl");
        let buffer = "function f(x)\n    @show x + 1\nend\n";
        let mut base = module("Base", &["map"]);
        base.functions.push(function("map"));
        let lib = library(vec![package(base)]);
        let expected = compute_semantic_tokens(buffer, PositionEncoding::Utf8, &lib);
        assert!(!expected.data.is_empty(), "fixture must yield tokens");

        // Cache hit: tracked text == buffer → tokens off the cached tree.
        let mut db = IncrementalDatabase::default();
        db.set_library_packages(lib.clone());
        db.upsert_file(path, buffer.to_string());
        assert_eq!(
            semantic_tokens_via_db(&db.snapshot(), path, buffer, PositionEncoding::Utf8),
            expected,
            "cached-tree tokens must match the re-parse path"
        );

        // Stale db (tracked text lags the buffer) → fall back to a fresh parse.
        let mut stale = IncrementalDatabase::default();
        stale.set_library_packages(lib.clone());
        stale.upsert_file(path, "y = 1\n".to_string());
        assert_eq!(
            semantic_tokens_via_db(&stale.snapshot(), path, buffer, PositionEncoding::Utf8),
            expected,
            "version skew must fall back to the buffer text"
        );

        // Untracked path → fall back as well.
        let mut empty = IncrementalDatabase::default();
        empty.set_library_packages(lib);
        assert_eq!(
            semantic_tokens_via_db(&empty.snapshot(), path, buffer, PositionEncoding::Utf8),
            expected,
            "untracked path must fall back to the buffer text"
        );
    }
}
