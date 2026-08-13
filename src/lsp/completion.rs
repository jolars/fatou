//! Completion (`textDocument/completion` and `completionItem/resolve`).
//!
//! Four contexts, decided by a lexical backward scan from the cursor (robust to
//! the parser's error recovery on the partial input completion always sees, like
//! `Foo.` or `@t`):
//!
//! - **LaTeX** — the run follows a backslash (`\alpha`, `\_1`, `\:smile:`): the
//!   REPL's tab-substitution sequences, inserting the character they expand to;
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
    CompletionItem, CompletionItemKind, CompletionTextEdit, Documentation, MarkupContent,
    MarkupKind, Position, TextEdit,
};
use rowan::TextSize;
use serde::{Deserialize, Serialize};

use crate::incremental::Analysis;
use crate::index::{ModuleIndex, PackageIndex};
use crate::parser::{KEYWORDS, parse};
use crate::resolve::{
    Candidate, ModulePath, Namespace, PackageSource, Resolver, Source, resolve_submodule,
};
use crate::semantic::{BindingKind, SemanticModel};
use crate::syntax::{SyntaxKind, SyntaxNode};
use crate::text::{PositionEncoding, TextBuffer};

use super::latex_symbols::{EMOJI_SYMBOLS, LATEX_SYMBOLS};
use super::render::{binding_detail, function_detail, type_detail};
use super::symbols::token_at;

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
    let offset = TextBuffer::new(text).position_to_byte(position, encoding);
    let root = parse(text).cst;
    let model = SemanticModel::build(&root);
    // The pure path has no file path to key workspace membership on; the live
    // server passes the workspace module through `completion_via_db`.
    completions_for(
        &model,
        &root,
        packages,
        None,
        text,
        TextSize::new(offset as u32),
        encoding,
    )
}

/// Compute completions off the snapshot's cached parse when the db's tracked
/// buffer for `path` still matches `text`; otherwise re-parse. A write racing
/// the read trips `salsa::Cancelled`, which also falls back to a fresh parse.
/// Mirrors [`document_symbols_via_db`](super::symbols::document_symbols_via_db).
pub(crate) fn completion_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &TextBuffer,
    position: Position,
    encoding: PositionEncoding,
) -> Vec<CompletionItem> {
    let offset = TextSize::new(text.line_index().position_to_byte(position, encoding) as u32);
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            // The tracked input lags the live buffer; the cached model is stale.
            return None;
        }
        let root = snapshot.parsed_tree(file);
        let model = snapshot.semantic_model(file);
        let workspace = snapshot.workspace_member(path);
        Some(completions_for(
            model,
            &root,
            snapshot,
            workspace,
            &text.text(),
            offset,
            encoding,
        ))
    }));
    match cached {
        Ok(Some(items)) => items,
        // Cache miss (`Ok(None)`) or a racing write (`Err`): re-parse from text.
        Ok(None) | Err(_) => compute_completions(&text.text(), position, encoding, snapshot),
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
    root: &SyntaxNode,
    packages: &P,
    workspace: Option<(Arc<PackageIndex>, ModulePath)>,
    text: &str,
    offset: TextSize,
    encoding: PositionEncoding,
) -> Vec<CompletionItem> {
    let offset_bytes: usize = offset.into();
    match context_at(text, offset_bytes) {
        // A sequence is an input method, not code, so it wins over the
        // identifier run it would otherwise look like. Where the backslash is
        // more likely the literal's own than an input sequence, nothing is
        // offered at all rather than falling back to another context: a popup
        // of names over a regex escape would be just as much in the way.
        Context::Latex { start } => {
            let typed = &text[start..offset_bytes];
            let suppressed = match string_context_at(root, offset) {
                StringContext::Verbatim => true,
                StringContext::Plain => is_lone_escape(&typed[1..]),
                StringContext::Code => false,
            };
            if suppressed {
                Vec::new()
            } else {
                latex_items(text, start, offset_bytes, encoding)
            }
        }
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
    /// Inside a LaTeX or emoji input sequence: `start` is the byte offset of the
    /// opening backslash, so the sequence typed so far is `text[start..offset]`.
    Latex {
        start: usize,
    },
}

/// Classify the cursor at byte `offset` by scanning the identifier run and the
/// punctuation just before it.
fn context_at(text: &str, offset: usize) -> Context {
    let prefix = &text[..offset.min(text.len())];
    if let Some(start) = sequence_start(prefix) {
        return Context::Latex { start };
    }
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

// --- LaTeX and emoji sequences ---------------------------------------------

/// The longest key in either table, capping the backward scan so a stray
/// backslash far up the line cannot pull in an unbounded run. Pinned by
/// [`tables_fit_the_scanner`](tests::tables_fit_the_scanner).
const MAX_SEQUENCE_LEN: usize = 43;

/// The byte offset of the backslash opening the input sequence `prefix` ends
/// in, or `None` when it does not end in one.
///
/// The scan stops at anything a key cannot contain, so a backslash is only
/// found across sequence characters. Note that a backslash directly after an
/// operand is *not* excluded: `A\b` is left division, but `x\_1` is how one
/// types `x₁`, and the second is far and away the common case.
fn sequence_start(prefix: &str) -> Option<usize> {
    for (i, c) in prefix.char_indices().rev() {
        if prefix.len() - i > MAX_SEQUENCE_LEN {
            return None;
        }
        if c == '\\' {
            return Some(i);
        }
        if !is_sequence_char(c) {
            return None;
        }
    }
    None
}

/// Whether `c` can appear in a sequence key after its backslash. Every key is
/// ASCII: letters and digits, plus the punctuation the sub/superscript,
/// fraction, and emoji keys use (`\^2`, `\_1`, `\1/2`, `\:+1:`).
fn is_sequence_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_^/+-()!=<>:".contains(c)
}

/// The items for the sequence `text[start..offset]`, each replacing the whole
/// sequence (backslash included) with the character it expands to.
fn latex_items(
    text: &str,
    start: usize,
    offset: usize,
    encoding: PositionEncoding,
) -> Vec<CompletionItem> {
    let typed = &text[start..offset];
    let line_index = TextBuffer::new(text);
    let range = lsp_types::Range {
        start: line_index.byte_to_position(start, encoding),
        end: line_index.byte_to_position(offset, encoding),
    };
    // The REPL only reaches for emoji once the `:` is there, so a bare `\` does
    // not bury 2549 LaTeX sequences under 1242 emoji.
    let emoji: &[(&str, &str)] = if typed.starts_with("\\:") {
        prefixed(EMOJI_SYMBOLS, typed)
    } else {
        &[]
    };
    prefixed(LATEX_SYMBOLS, typed)
        .iter()
        .chain(emoji)
        .map(|&(sequence, expansion)| latex_item(sequence, expansion, range))
        .collect()
}

/// The run of entries whose key starts with `prefix`, found by binary search on
/// the sorted table.
fn prefixed<'a>(table: &'a [(&'a str, &'a str)], prefix: &str) -> &'a [(&'a str, &'a str)] {
    let start = table.partition_point(|(key, _)| *key < prefix);
    let len = table[start..].partition_point(|(key, _)| key.starts_with(prefix));
    &table[start..start + len]
}

/// An item offering `sequence`, inserting `expansion` over `range`. The label
/// keeps the backslash so the client filters against what was actually typed.
fn latex_item(sequence: &str, expansion: &str, range: lsp_types::Range) -> CompletionItem {
    CompletionItem {
        label: sequence.to_string(),
        kind: Some(CompletionItemKind::TEXT),
        detail: Some(expansion.to_string()),
        documentation: Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: format!("`{expansion}` {}", codepoints(expansion)),
        })),
        filter_text: Some(sequence.to_string()),
        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
            range,
            new_text: expansion.to_string(),
        })),
        ..Default::default()
    }
}

/// The `U+` code points of `s`, space-separated (a couple of dozen expansions
/// are a base character plus a combining mark).
fn codepoints(s: &str) -> String {
    s.chars()
        .map(|c| format!("U+{:04X}", c as u32))
        .collect::<Vec<_>>()
        .join(" ")
}

/// What kind of literal the cursor sits in, which decides how much of a
/// backslash run is the string's own text.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum StringContext {
    /// Ordinary code, where a backslash is never an escape.
    Code,
    /// A plain string or docstring: prose, where writing `α` is exactly what
    /// the sequences are for, but where `\n` is also a newline.
    Plain,
    /// A prefixed string macro (`r"\d"`, `raw"\n"`) or a command literal, where
    /// every backslash is the literal's own and a popup over each regex escape
    /// would be pure noise.
    Verbatim,
}

fn string_context_at(root: &SyntaxNode, offset: TextSize) -> StringContext {
    let Some(token) = token_at(root, offset) else {
        return StringContext::Code;
    };
    token
        .parent_ancestors()
        .find_map(|node| match node.kind() {
            SyntaxKind::CMD_LITERAL => Some(StringContext::Verbatim),
            SyntaxKind::STRING_LITERAL => Some(
                if node
                    .children_with_tokens()
                    .any(|c| c.kind() == SyntaxKind::STRING_PREFIX)
                {
                    StringContext::Verbatim
                } else {
                    StringContext::Plain
                },
            ),
            _ => None,
        })
        .unwrap_or(StringContext::Code)
}

/// Whether `sequence` is a lone backslash-escape the Julia lexer would read as
/// one, like `\n` or `\u`.
///
/// Only consulted inside a plain string, and only for a one-character run: a
/// popup headed by `\nabla` the instant someone types `"\n` would be in the way
/// far more often than it helps. One more character disambiguates (`\na` is
/// nobody's escape), so `\nu` and `\alpha` are still one keystroke behind.
fn is_lone_escape(sequence: &str) -> bool {
    let mut chars = sequence.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return false;
    };
    // The escapes Julia's lexer accepts after a backslash, per `read_escaped`
    // in JuliaSyntax's tokenizer.
    c.is_ascii_digit() || "abefnrtv\\'\"?xuU".contains(c)
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
            usings: Vec::new(),
            imported_names: Vec::new(),
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
        let line_index = TextBuffer::new(src);
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
        let root = parse(src).cst;
        let model = SemanticModel::build(&root);
        let offset = TextSize::new((src.find(needle).unwrap() + needle.len()) as u32);
        completions_for(
            &model,
            &root,
            lib,
            Some((workspace, Vec::new())),
            src,
            offset,
            PositionEncoding::Utf16,
        )
    }

    /// Completions at the position just past `needle`, with no library at all.
    fn completions_bare(src: &str, needle: &str) -> Vec<CompletionItem> {
        completions_at(src, needle, &library(vec![]))
    }

    /// The item labelled `label`, panicking when it was not offered.
    fn item<'a>(items: &'a [CompletionItem], label: &str) -> &'a CompletionItem {
        items
            .iter()
            .find(|i| i.label == label)
            .unwrap_or_else(|| panic!("no {label:?} in {:?}", labels(items)))
    }

    /// The text an item inserts and the range it replaces.
    fn edit(item: &CompletionItem) -> (&str, lsp_types::Range) {
        match item.text_edit.as_ref().expect("a text edit") {
            CompletionTextEdit::Edit(e) => (e.new_text.as_str(), e.range),
            other => panic!("expected a plain edit, got {other:?}"),
        }
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
            type_args: Vec::new(),
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

    // --- LaTeX and emoji sequences -----------------------------------------

    /// The generated tables and the backward scanner have to agree: every key
    /// must be reachable by a scan, and both tables must be sorted for
    /// [`prefixed`]. Regenerating on a Julia bump re-checks this.
    #[test]
    fn tables_fit_the_scanner() {
        for table in [LATEX_SYMBOLS, EMOJI_SYMBOLS] {
            assert!(!table.is_empty());
            for window in table.windows(2) {
                assert!(window[0].0 < window[1].0, "unsorted at {:?}", window[0].0);
            }
            for &(key, expansion) in table {
                let rest = key
                    .strip_prefix('\\')
                    .unwrap_or_else(|| panic!("{key:?} lacks a backslash"));
                assert!(!rest.is_empty(), "{key:?} is a bare backslash");
                assert!(
                    rest.chars().all(is_sequence_char),
                    "{key:?} has a character the scanner stops at"
                );
                assert!(
                    key.len() <= MAX_SEQUENCE_LEN,
                    "{key:?} outruns the scan cap"
                );
                assert!(!expansion.is_empty(), "{key:?} expands to nothing");
            }
        }
    }

    #[test]
    fn latex_sequence_offers_expansions_and_replaces_the_backslash() {
        let items = completions_bare("x = \\alph", "\\alph");
        let alpha = item(&items, "\\alpha");
        assert_eq!(alpha.detail.as_deref(), Some("α"));
        assert_eq!(alpha.kind, Some(CompletionItemKind::TEXT));
        let (new_text, range) = edit(alpha);
        assert_eq!(new_text, "α");
        // The edit swallows the backslash, so accepting leaves `x = α`.
        assert_eq!(range.start.character, 4);
        assert_eq!(range.end.character, 9);
        // Only the matching prefix is offered.
        assert!(!labels(&items).contains(&"\\beta".to_string()));
    }

    /// The REPL way to type `x₁` puts the backslash straight after an operand,
    /// which is also how left division (`A\b`) is written. The subscript wins:
    /// a completion only inserts when accepted, and this is the common case.
    #[test]
    fn sequence_directly_after_an_identifier_is_offered() {
        let items = completions_bare("x\\_1", "\\_1");
        let (new_text, range) = edit(item(&items, "\\_1"));
        assert_eq!(new_text, "₁");
        // The edit starts at the backslash, leaving the `x` alone.
        assert_eq!(range.start.character, 1);
    }

    #[test]
    fn emoji_sequences_are_offered() {
        let items = completions_bare("\\:smi", "\\:smi");
        assert_eq!(edit(item(&items, "\\:smile:")).0, "😄");
    }

    /// `REPL.REPLCompletions.completions` answers a bare `\` with the LaTeX
    /// table alone and only reaches the emoji once the `:` is typed. Matching
    /// that keeps the first list the muscle-memory one.
    #[test]
    fn a_bare_backslash_offers_latex_only() {
        let names = labels(&completions_bare("\\", "\\"));
        assert!(names.contains(&"\\alpha".to_string()));
        assert!(!names.contains(&"\\:smile:".to_string()));
        assert_eq!(names.len(), LATEX_SYMBOLS.len());
    }

    #[test]
    fn a_colon_opens_the_emoji_table() {
        let names = labels(&completions_bare("\\:", "\\:"));
        assert_eq!(names.len(), EMOJI_SYMBOLS.len());
        assert!(names.contains(&"\\:smile:".to_string()));
    }

    /// In `r"\d"` or `raw"\n"` the backslash is the string's own, so a popup of
    /// `\delta`/`\nabla` over every regex would be pure noise.
    #[test]
    fn verbatim_strings_offer_nothing() {
        for src in [
            "m = r\"\\d",
            "p = raw\"\\n",
            "c = `ls \\d",
            "m = match(r\"\\s", // still unterminated, mid-typing
        ] {
            let needle = &src[src.len() - 2..];
            assert!(
                completions_bare(src, needle).is_empty(),
                "expected nothing in {src:?}"
            );
        }
    }

    /// A plain string and a docstring are prose, where `α` is exactly what the
    /// author wants; only the string macros are verbatim.
    #[test]
    fn plain_strings_and_docstrings_still_offer_sequences() {
        for src in ["s = \"\\alph", "\"\"\"\n\\alph"] {
            let names = labels(&completions_bare(src, "\\alph"));
            assert!(
                names.contains(&"\\alpha".to_string()),
                "expected sequences in {src:?}, got {names:?}"
            );
        }
    }

    /// In a plain string `\n` is a newline, so a one-character escape run stays
    /// quiet; the second character resolves the ambiguity and the sequences
    /// come back. In code there is no escape to collide with.
    #[test]
    fn a_lone_escape_stays_quiet_only_inside_a_plain_string() {
        assert!(completions_bare("s = \"\\n", "\\n").is_empty());
        assert!(completions_bare("s = \"\\u", "\\u").is_empty());
        // One more character and `\nu` is offered again.
        let names = labels(&completions_bare("s = \"\\nu", "\\nu"));
        assert!(names.contains(&"\\nu".to_string()), "{names:?}");
        // A non-escape character is never ambiguous, even at one character.
        assert!(!completions_bare("s = \"\\^", "\\^").is_empty());
        // The same run in code is not an escape at all.
        assert!(!completions_bare("x = \\n", "\\n").is_empty());
    }

    #[test]
    fn lone_escape_recognition() {
        for escape in ["n", "t", "u", "x", "0", "\\", "\""] {
            assert!(is_lone_escape(escape), "{escape:?} is an escape");
        }
        for other in ["^", "_", ":", "alpha", "nu", "", "nn"] {
            assert!(!is_lone_escape(other), "{other:?} is not a lone escape");
        }
    }

    /// Candidate sets taken from `REPL.REPLCompletions.completions` on Julia
    /// 1.12, so the two agree on what a partial sequence offers.
    #[test]
    fn candidate_counts_match_the_repl() {
        for (typed, expected) in [
            ("\\alph", 1),
            ("\\:smi", 9),
            ("\\:smile", 4),
            ("\\:smile:", 1),
            ("\\:+1:", 1),
        ] {
            let src = format!("x = {typed}");
            let items = completions_bare(&src, typed);
            assert_eq!(
                items.len(),
                expected,
                "{typed:?} offered {:?}",
                labels(&items)
            );
        }
    }

    #[test]
    fn an_unknown_sequence_offers_nothing() {
        assert!(completions_bare("\\notasymbol", "\\notasymbol").is_empty());
    }

    #[test]
    fn prefix_search_takes_exactly_the_matching_run() {
        let table: &[(&str, &str)] = &[("\\a", "1"), ("\\ab", "2"), ("\\abc", "3"), ("\\b", "4")];
        assert_eq!(prefixed(table, "\\ab").len(), 2);
        assert_eq!(prefixed(table, "\\").len(), 4);
        assert_eq!(prefixed(table, "\\abcd").len(), 0);
        assert_eq!(prefixed(table, "\\z").len(), 0);
    }

    #[test]
    fn codepoints_names_combining_expansions() {
        assert_eq!(codepoints("α"), "U+03B1");
        // `\nleqslant` is a base character plus a combining solidus.
        assert_eq!(codepoints("⩽̸"), "U+2A7D U+0338");
    }

    #[test]
    fn context_detection() {
        assert_eq!(context_at("foo", 3), Context::Value);
        assert_eq!(context_at("\\alph", 5), Context::Latex { start: 0 });
        assert_eq!(context_at("x = \\_1", 7), Context::Latex { start: 4 });
        assert_eq!(context_at("\\", 1), Context::Latex { start: 0 });
        // A spaced left division is not a sequence: the scan stops at the space.
        assert_eq!(context_at("A \\ b", 5), Context::Value);
        // Nor is a run longer than the longest key.
        let long = format!("\\{}", "a".repeat(MAX_SEQUENCE_LEN));
        assert_eq!(context_at(&long, long.len()), Context::Value);
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
