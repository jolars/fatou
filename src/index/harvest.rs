//! The harvest walk: parse a package's `src/` with fatou's parser, follow
//! static `include()` chains to build the module tree, and extract the public
//! API surface into a [`PackageIndex`].
//!
//! The walk threads two things through recursion: the file currently being read
//! (for `include` resolution and source locations) and a `&mut ModuleIndex`
//! destination — items splice into the module that lexically contains them, so
//! `module Sub` nests and an `include` inside it lands in `Sub`. Harvesting is
//! best-effort: unreadable files, unresolved includes, parse errors, and
//! include cycles are recorded as [`HarvestDiagnostic`]s and the walk continues.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use rowan::TextRange;

use crate::ast::{AstNode, AstToken, CallExpr, Expr, HasArgList};
use crate::project::{include_target, resolve_target};
use crate::semantic::signature::{annotation_parts, has_call_core, peel_signature, type_name_of};
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

use super::model::*;
use super::typeexpr::{TypeExpr, lower_type, lower_type_params, normalized_text};

/// Harvest the package rooted at `source_root` (the directory containing
/// `src/`), taking the package name from the directory's file name.
pub fn harvest_package(source_root: &Path) -> PackageIndex {
    let name = source_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    harvest_package_named(source_root, &name)
}

/// Harvest the package named `name` rooted at `source_root`, entering at
/// `src/<name>.jl`.
pub fn harvest_package_named(source_root: &Path, name: &str) -> PackageIndex {
    let entry = source_root.join("src").join(format!("{name}.jl"));
    harvest_entry(source_root, &entry, name)
}

/// Harvest the module named `name` rooted at `source_root`, entering at the
/// explicit file `entry`. Locations stay relative to `source_root`, so callers
/// with a non-`src/` layout (Julia's Base at `base/Base.jl`, Core at
/// `base/boot.jl`) get the same depot-relative `DefLocation`s as ordinary
/// packages entered through [`harvest_package_named`].
pub fn harvest_entry(source_root: &Path, entry: &Path, name: &str) -> PackageIndex {
    let mut harvester = Harvester {
        root: source_root.to_path_buf(),
        visited: HashSet::new(),
        members: Vec::new(),
        member_modules: BTreeMap::new(),
        module_path: Vec::new(),
        diagnostics: Vec::new(),
        root_filled: false,
    };
    let mut root = ModuleIndex {
        name: name.to_string(),
        bare: false,
        loc: DefLocation {
            file: harvester.relative(entry),
            range: Span { start: 0, end: 0 },
        },
        exports: Vec::new(),
        functions: Vec::new(),
        types: Vec::new(),
        consts: Vec::new(),
        macros: Vec::new(),
        submodules: Vec::new(),
    };

    match std::fs::read_to_string(entry) {
        Ok(text) => {
            harvester.visited.insert(canonical(entry));
            harvester.walk_text(&text, entry, &mut root, true);
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            harvester
                .diagnostics
                .push(HarvestDiagnostic::EntryFileMissing {
                    path: entry.to_path_buf(),
                });
        }
        Err(err) => harvester.diagnostics.push(HarvestDiagnostic::ReadError {
            path: entry.to_path_buf(),
            message: err.to_string(),
        }),
    }

    PackageIndex {
        name: name.to_string(),
        root,
        members: harvester.members,
        member_modules: harvester.member_modules,
        diagnostics: harvester.diagnostics,
    }
}

struct Harvester {
    /// The package source root, for relativizing [`DefLocation::file`].
    root: PathBuf,
    /// Canonicalized files already walked — the cycle and duplicate guard.
    visited: HashSet<PathBuf>,
    /// Every source file walked, package-relative and in walk order. Recorded
    /// once per unique file (the `visited` guard runs before [`walk_text`]).
    members: Vec<PathBuf>,
    /// The host module path of each member (see
    /// [`PackageIndex::member_modules`]), captured from [`module_path`] at the
    /// point the file is first walked.
    member_modules: BTreeMap<PathBuf, Vec<String>>,
    /// The current nested-`module` path (relative to the root module, empty at
    /// root), pushed/popped as [`handle_module`](Self::handle_module) recurses.
    module_path: Vec<String>,
    diagnostics: Vec<HarvestDiagnostic>,
    /// Whether the synthesized root module has been matched to a literal
    /// `module <Name>` in the source yet.
    root_filled: bool,
}

impl Harvester {
    /// `file` relative to the package root, falling back to `file` as-is.
    fn relative(&self, file: &Path) -> PathBuf {
        file.strip_prefix(&self.root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| file.to_path_buf())
    }

    fn loc(&self, file: &Path, range: TextRange) -> DefLocation {
        DefLocation {
            file: self.relative(file),
            range: range.into(),
        }
    }

    /// Read `file`, splicing its top-level items into `dest`. `at_root` is true
    /// while walking the entry file's own top level (before the package's
    /// `module <Name>` has been matched).
    fn walk_file(&mut self, file: &Path, dest: &mut ModuleIndex, at_root: bool) {
        let key = canonical(file);
        if !self.visited.insert(key) {
            // Already walked (cycle or duplicate include): walk it once.
            self.diagnostics.push(HarvestDiagnostic::IncludeCycle {
                path: file.to_path_buf(),
            });
            return;
        }
        match std::fs::read_to_string(file) {
            Ok(text) => self.walk_text(&text, file, dest, at_root),
            Err(err) => self.diagnostics.push(HarvestDiagnostic::ReadError {
                path: file.to_path_buf(),
                message: err.to_string(),
            }),
        }
    }

    fn walk_text(&mut self, text: &str, file: &Path, dest: &mut ModuleIndex, at_root: bool) {
        let rel = self.relative(file);
        self.member_modules
            .insert(rel.clone(), self.module_path.clone());
        self.members.push(rel);
        let parsed = crate::parser::parse(text);
        if !parsed.diagnostics.is_empty() {
            self.diagnostics.push(HarvestDiagnostic::ParseError {
                path: file.to_path_buf(),
                count: parsed.diagnostics.len(),
            });
        }
        for child in parsed.cst.children() {
            self.walk_item(&child, file, dest, at_root, None);
        }
    }

    /// Dispatch one top-level item. `pending_doc` is a docstring captured from a
    /// preceding `DOC` node or `@doc` call, consumed by the documented target.
    fn walk_item(
        &mut self,
        node: &SyntaxNode,
        file: &Path,
        dest: &mut ModuleIndex,
        at_root: bool,
        pending_doc: Option<Docstring>,
    ) {
        match node.kind() {
            SyntaxKind::DOC => self.walk_doc(node, file, dest, at_root),
            SyntaxKind::MACRO_CALL => self.walk_macro_call(node, file, dest, at_root, pending_doc),
            SyntaxKind::CALL_EXPR => self.walk_call(node, file, dest, at_root),
            SyntaxKind::FUNCTION_DEF => self.handle_function(node, file, dest, false, pending_doc),
            SyntaxKind::ASSIGNMENT_EXPR => {
                if is_short_form_def(node) {
                    self.handle_short_form(node, file, dest, pending_doc);
                }
            }
            SyntaxKind::MACRO_DEF => self.handle_macro_def(node, file, dest, pending_doc),
            SyntaxKind::STRUCT_DEF | SyntaxKind::ABSTRACT_DEF | SyntaxKind::PRIMITIVE_DEF => {
                self.handle_type(node, file, dest, pending_doc);
            }
            SyntaxKind::MODULE_DEF => self.handle_module(node, file, dest, at_root),
            SyntaxKind::EXPORT_STMT | SyntaxKind::PUBLIC_STMT => {
                self.handle_name_list(node, file, dest);
            }
            SyntaxKind::CONST_STMT => self.handle_const(node, file, dest, pending_doc),
            _ => {}
        }
    }

    /// A `DOC` node `(doc <string> <target>)`: capture the docstring and walk
    /// the target as its documented definition.
    fn walk_doc(&mut self, node: &SyntaxNode, file: &Path, dest: &mut ModuleIndex, at_root: bool) {
        let mut children = node.children();
        let Some(string) = children.next() else {
            return;
        };
        let doc = self.docstring_from(&string, file);
        if let Some(target) = children.next() {
            self.walk_item(&target, file, dest, at_root, doc);
        }
    }

    /// A macro call: `@doc`/`Base.@doc` documents its target; `@kwdef` unwraps
    /// to a struct; any other macro whose argument is itself a definition is a
    /// transparent wrapper (`@inline f() = ...`) that we recurse through.
    fn walk_macro_call(
        &mut self,
        node: &SyntaxNode,
        file: &Path,
        dest: &mut ModuleIndex,
        at_root: bool,
        pending_doc: Option<Docstring>,
    ) {
        let name = macro_call_name(node);
        let args = macro_args(node);
        match name.as_deref() {
            Some("doc") => {
                // `@doc "str" target`: the doc string then the documented item.
                let doc = args.first().and_then(|s| self.docstring_from(s, file));
                if let Some(target) = args.get(1) {
                    self.walk_item(target, file, dest, at_root, doc);
                }
            }
            Some("kwdef") => {
                if let Some(target) = args.first() {
                    self.walk_item(target, file, dest, at_root, pending_doc);
                }
            }
            _ => {
                // Transparent wrapper: recurse only into definition-shaped
                // arguments, never into blocks (which would harvest a macro's
                // internal statements as if they were API).
                for arg in &args {
                    if is_definition(arg.kind()) {
                        self.walk_item(arg, file, dest, at_root, pending_doc.clone());
                    }
                }
            }
        }
    }

    /// A bare call: follow a static `include("literal")`; report a dynamic or
    /// unreadable include; ignore any other call.
    fn walk_call(&mut self, node: &SyntaxNode, file: &Path, dest: &mut ModuleIndex, at_root: bool) {
        let Some(call) = CallExpr::cast(node.clone()) else {
            return;
        };
        if let Some(raw) = include_target(&call) {
            match resolve_target(&raw, file.parent()) {
                Some(target) if target.is_file() => self.walk_file(&target, dest, at_root),
                _ => self.diagnostics.push(HarvestDiagnostic::UnresolvedInclude {
                    raw,
                    from: file.to_path_buf(),
                }),
            }
        } else if is_include_callee(&call) {
            // A dynamic/interpolated/qualified `include` we cannot resolve.
            self.diagnostics.push(HarvestDiagnostic::UnresolvedInclude {
                raw: call
                    .arg_list()
                    .map(|l| normalized_text(l.syntax()))
                    .unwrap_or_default(),
                from: file.to_path_buf(),
            });
        }
    }

    fn handle_module(
        &mut self,
        node: &SyntaxNode,
        file: &Path,
        dest: &mut ModuleIndex,
        at_root: bool,
    ) {
        let bare = has_token(node, SyntaxKind::BAREMODULE_KW);
        let Some((name, range)) = module_name(node) else {
            return;
        };
        let block = node.children().find(|c| c.kind() == SyntaxKind::BLOCK);

        // The package's own `module <Name>` absorbs into the synthesized root
        // rather than nesting a module of the same name inside it.
        if at_root && !self.root_filled && name == dest.name {
            self.root_filled = true;
            dest.bare = bare;
            dest.loc = self.loc(file, range);
            if let Some(block) = block {
                for child in block.children() {
                    self.walk_item(&child, file, dest, false, None);
                }
            }
            return;
        }

        let mut child = ModuleIndex {
            name: name.clone(),
            bare,
            loc: self.loc(file, range),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            consts: Vec::new(),
            macros: Vec::new(),
            submodules: Vec::new(),
        };
        self.module_path.push(name);
        if let Some(block) = block {
            for item in block.children() {
                self.walk_item(&item, file, &mut child, false, None);
            }
        }
        self.module_path.pop();
        dest.submodules.push(child);
    }

    fn handle_function(
        &mut self,
        node: &SyntaxNode,
        file: &Path,
        dest: &mut ModuleIndex,
        _short: bool,
        doc: Option<Docstring>,
    ) {
        let Some(start) = signature_start(node) else {
            return;
        };
        self.add_method(start, file, dest, doc);
    }

    fn handle_short_form(
        &mut self,
        node: &SyntaxNode,
        file: &Path,
        dest: &mut ModuleIndex,
        doc: Option<Docstring>,
    ) {
        // The signature is the assignment's left-hand side.
        let Some(lhs) = node.children().next() else {
            return;
        };
        self.add_method(lhs, file, dest, doc);
    }

    /// Build a [`Method`] from a signature-position node and file it under its
    /// `(owner, name)` group in `dest`.
    fn add_method(
        &mut self,
        start: SyntaxNode,
        file: &Path,
        dest: &mut ModuleIndex,
        doc: Option<Docstring>,
    ) {
        let (core, wheres, return_ty) = peel_signature(start);
        let Some(core) = core else {
            return;
        };
        let where_clauses = lower_type_params(wheres.iter());
        let return_type = return_ty.as_ref().map(lower_type);

        let (name, owner, name_range, params, keyword_params, has_body) = match core.kind() {
            SyntaxKind::CALL_EXPR => {
                let Some((name, owner, name_range)) = callee_name(&core) else {
                    return;
                };
                let (params, keyword_params) = extract_params(&core);
                (name, owner, name_range, params, keyword_params, true)
            }
            // `function f end`: a bodyless declaration with no methods yet.
            SyntaxKind::NAME | SyntaxKind::NONSTANDARD_IDENTIFIER => {
                let Some(name) = node_name_text(&core) else {
                    return;
                };
                (name, None, core.text_range(), Vec::new(), Vec::new(), false)
            }
            _ => return,
        };

        let method = Method {
            params,
            keyword_params,
            where_clauses,
            return_type,
            has_body,
            doc,
            loc: self.loc(file, name_range),
        };
        push_method(dest, name, owner, method);
    }

    fn handle_macro_def(
        &mut self,
        node: &SyntaxNode,
        file: &Path,
        dest: &mut ModuleIndex,
        doc: Option<Docstring>,
    ) {
        let Some(start) = signature_start(node) else {
            return;
        };
        // A macro signature is a call whose callee is the macro name.
        if start.kind() != SyntaxKind::CALL_EXPR {
            return;
        }
        let Some((name, _owner, name_range)) = callee_name(&start) else {
            return;
        };
        let (params, _kw) = extract_params(&start);
        dest.macros.push(MacroDef {
            name: format!("@{name}"),
            params,
            doc,
            loc: self.loc(file, name_range),
        });
    }

    fn handle_type(
        &mut self,
        node: &SyntaxNode,
        file: &Path,
        dest: &mut ModuleIndex,
        doc: Option<Docstring>,
    ) {
        let kind = match node.kind() {
            SyntaxKind::STRUCT_DEF => TypeKind::Struct {
                mutable: has_token(node, SyntaxKind::MUTABLE_KW),
            },
            SyntaxKind::ABSTRACT_DEF => TypeKind::Abstract,
            _ => TypeKind::Primitive {
                bits: primitive_bits(node),
            },
        };
        let Some(start) = signature_start(node) else {
            return;
        };
        let header = header_parts(&start);
        let Some((name, name_range)) = header.name else {
            return;
        };

        let mut fields = Vec::new();
        if let Some(block) = node.children().find(|c| c.kind() == SyntaxKind::BLOCK) {
            self.collect_struct_body(&block, &name, file, dest, &mut fields);
        }

        dest.types.push(TypeDef {
            name,
            kind,
            type_params: header.type_params,
            supertype: header.supertype,
            fields,
            doc,
            loc: self.loc(file, name_range),
        });
    }

    /// Walk a struct body: bare/annotated members become [`Field`]s; inner
    /// constructors (and any nested definition) file under the enclosing
    /// module's group for the struct name.
    fn collect_struct_body(
        &mut self,
        block: &SyntaxNode,
        struct_name: &str,
        file: &Path,
        dest: &mut ModuleIndex,
        fields: &mut Vec<Field>,
    ) {
        for child in block.children() {
            match child.kind() {
                SyntaxKind::NAME | SyntaxKind::NONSTANDARD_IDENTIFIER => {
                    if let Some(name) = node_name_text(&child) {
                        fields.push(Field {
                            name,
                            type_annotation: None,
                            default: None,
                        });
                    }
                }
                SyntaxKind::TYPE_ANNOTATION => push_annotated_field(&child, None, fields),
                SyntaxKind::CONST_STMT => {
                    // `const x::T` field marker.
                    if let Some(inner) = child.children().next() {
                        match inner.kind() {
                            SyntaxKind::TYPE_ANNOTATION => {
                                push_annotated_field(&inner, None, fields);
                            }
                            SyntaxKind::NAME => {
                                if let Some(name) = node_name_text(&inner) {
                                    fields.push(Field {
                                        name,
                                        type_annotation: None,
                                        default: None,
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }
                SyntaxKind::ASSIGNMENT_EXPR => {
                    if is_short_form_def(&child) {
                        // Inner constructor short form.
                        self.handle_short_form(&child, file, dest, None);
                    } else if let Some(lhs) = child.children().next() {
                        // `@kwdef` field with a default.
                        let default = child.children().nth(1).map(|v| normalized_text(&v));
                        match lhs.kind() {
                            SyntaxKind::TYPE_ANNOTATION => {
                                push_annotated_field(&lhs, default, fields);
                            }
                            SyntaxKind::NAME | SyntaxKind::NONSTANDARD_IDENTIFIER => {
                                if let Some(name) = node_name_text(&lhs) {
                                    fields.push(Field {
                                        name,
                                        type_annotation: None,
                                        default,
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }
                // Inner constructors and other nested definitions.
                SyntaxKind::FUNCTION_DEF => self.handle_function(&child, file, dest, false, None),
                SyntaxKind::MACRO_CALL => {
                    self.walk_macro_call(&child, file, dest, false, None);
                }
                _ => {}
            }
            let _ = struct_name;
        }
    }

    fn handle_const(
        &mut self,
        node: &SyntaxNode,
        file: &Path,
        dest: &mut ModuleIndex,
        doc: Option<Docstring>,
    ) {
        // `const global x` nests the declaration in a GLOBAL_STMT.
        let inner = match node.children().next() {
            Some(n) if n.kind() == SyntaxKind::GLOBAL_STMT => n.children().next(),
            other => other,
        };
        let Some(inner) = inner else {
            return;
        };

        let (names, value_repr) = if inner.kind() == SyntaxKind::ASSIGNMENT_EXPR {
            let mut parts = inner.children();
            let lhs = parts.next();
            let value = parts.next();
            let names = lhs.map(|n| collect_names(&n)).unwrap_or_default();
            // A value preview is only unambiguous for a single-name const.
            let value_repr = match (names.len(), value) {
                (1, Some(v)) => Some(truncate(&normalized_text(&v), 120)),
                _ => None,
            };
            (names, value_repr)
        } else {
            (collect_names(&inner), None)
        };

        for (i, (name, range)) in names.into_iter().enumerate() {
            dest.consts.push(ConstDef {
                name,
                value_repr: if i == 0 { value_repr.clone() } else { None },
                doc: if i == 0 { doc.clone() } else { None },
                loc: self.loc(file, range),
            });
        }
    }

    fn handle_name_list(&mut self, node: &SyntaxNode, file: &Path, dest: &mut ModuleIndex) {
        let visibility = if node.kind() == SyntaxKind::EXPORT_STMT {
            Visibility::Exported
        } else {
            Visibility::Public
        };
        // `public` is a contextual keyword: skip the leading IDENT.
        let mut skip_keyword = node.kind() == SyntaxKind::PUBLIC_STMT;
        for element in node.children_with_tokens() {
            match element {
                rowan::NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::IDENT if skip_keyword => skip_keyword = false,
                    SyntaxKind::IDENT => {
                        self.push_export(dest, t.text(), t.text_range(), visibility, file);
                    }
                    k if k.is_operator() => {
                        self.push_export(dest, t.text(), t.text_range(), visibility, file);
                    }
                    _ => {}
                },
                rowan::NodeOrToken::Node(n) => match n.kind() {
                    SyntaxKind::MACRO_NAME => {
                        if let Some(t) = last_ident(&n) {
                            let name = format!("@{}", t.text());
                            self.push_export(dest, &name, n.text_range(), visibility, file);
                        }
                    }
                    SyntaxKind::PAREN_EXPR => {
                        let mut inner = n.children();
                        if let (Some(name), None) = (inner.next(), inner.next())
                            && name.kind() == SyntaxKind::NAME
                            && let Some(text) = node_name_text(&name)
                        {
                            self.push_export(dest, &text, name.text_range(), visibility, file);
                        }
                    }
                    _ => {}
                },
            }
        }
    }

    fn push_export(
        &self,
        dest: &mut ModuleIndex,
        name: &str,
        range: TextRange,
        visibility: Visibility,
        file: &Path,
    ) {
        dest.exports.push(ExportedName {
            name: name.to_string(),
            visibility,
            loc: self.loc(file, range),
        });
    }

    /// Build a [`Docstring`] from a string-literal node.
    fn docstring_from(&self, node: &SyntaxNode, file: &Path) -> Option<Docstring> {
        if node.kind() != SyntaxKind::STRING_LITERAL {
            return None;
        }
        let text: String = node
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .filter(|t| t.kind() == SyntaxKind::STRING_CONTENT)
            .map(|t| t.text().to_string())
            .collect();
        Some(Docstring {
            text,
            loc: self.loc(file, node.text_range()),
        })
    }
}

// --- free helpers -----------------------------------------------------------

/// Canonicalize a path for deduplication, falling back to a lexical
/// normalization when the file cannot be canonicalized.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| crate::incremental::normalize_path(path))
}

/// The first child of a definition's `SIGNATURE` node.
fn signature_start(def: &SyntaxNode) -> Option<SyntaxNode> {
    def.children()
        .find(|c| c.kind() == SyntaxKind::SIGNATURE)?
        .children()
        .next()
}

/// Whether an `ASSIGNMENT_EXPR` is a short-form function definition: a plain
/// `=` whose left-hand side peels to a call.
fn is_short_form_def(node: &SyntaxNode) -> bool {
    let Some(lhs) = node.children().next() else {
        return false;
    };
    has_call_core(&lhs) && has_token(node, SyntaxKind::EQ)
}

/// Whether `kind` is a definition node the harvester extracts (used to decide
/// whether to recurse through a transparent macro wrapper).
fn is_definition(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::FUNCTION_DEF
            | SyntaxKind::MACRO_DEF
            | SyntaxKind::STRUCT_DEF
            | SyntaxKind::ABSTRACT_DEF
            | SyntaxKind::PRIMITIVE_DEF
            | SyntaxKind::MODULE_DEF
            | SyntaxKind::CONST_STMT
            | SyntaxKind::ASSIGNMENT_EXPR
            | SyntaxKind::MACRO_CALL
    )
}

/// The defined name, owning module path, and name range for a `CALL_EXPR`
/// signature core. `Base.show` yields `("show", Some(["Base"]), range)`.
fn callee_name(call: &SyntaxNode) -> Option<(String, Option<Vec<String>>, TextRange)> {
    let callee = call
        .children_with_tokens()
        .find(|el| !is_trivia(el.kind()))?;
    match callee {
        rowan::NodeOrToken::Token(token) => {
            // A bare operator definition (`function +(a, b)`).
            Some((token.text().to_string(), None, token.text_range()))
        }
        rowan::NodeOrToken::Node(node) => match node.kind() {
            SyntaxKind::NAME | SyntaxKind::NONSTANDARD_IDENTIFIER => {
                Some((node_name_text(&node)?, None, node.text_range()))
            }
            SyntaxKind::CURLY_EXPR => {
                let base = type_name_of(&node)?;
                Some((node_name_text(&base)?, None, base.text_range()))
            }
            SyntaxKind::BINARY_EXPR => {
                let (path, last) = qualified_path(&node)?;
                let name = path.last()?.clone();
                let owner = path[..path.len() - 1].to_vec();
                Some((
                    name,
                    (!owner.is_empty()).then_some(owner),
                    last.text_range(),
                ))
            }
            _ => None,
        },
    }
}

/// Read a dotted `A.B.c` chain as its components plus the final `NAME` node
/// (`c`). Requires each level to be a dot access, so a plain operator call is
/// not misread as a qualified name.
fn qualified_path(node: &SyntaxNode) -> Option<(Vec<String>, SyntaxNode)> {
    let mut reversed = Vec::new();
    let mut cursor = node.clone();
    // The outermost `.`'s right operand is the final component; fix it once.
    let mut final_name: Option<SyntaxNode> = None;
    loop {
        if cursor.kind() != SyntaxKind::BINARY_EXPR || !has_token(&cursor, SyntaxKind::DOT) {
            return None;
        }
        let mut children = cursor.children();
        let lhs = children.next()?;
        let rhs = children.next()?;
        if rhs.kind() != SyntaxKind::NAME {
            return None;
        }
        if final_name.is_none() {
            final_name = Some(rhs.clone());
        }
        reversed.push(node_name_text(&rhs)?);
        match lhs.kind() {
            SyntaxKind::NAME => {
                reversed.push(node_name_text(&lhs)?);
                reversed.reverse();
                return Some((reversed, final_name?));
            }
            SyntaxKind::BINARY_EXPR => cursor = lhs,
            _ => return None,
        }
    }
}

/// The positional and keyword parameters of a `CALL_EXPR` signature.
fn extract_params(call: &SyntaxNode) -> (Vec<Param>, Vec<Param>) {
    let mut positional = Vec::new();
    let mut keyword = Vec::new();
    let Some(arg_list) = call.children().find(|c| c.kind() == SyntaxKind::ARG_LIST) else {
        return (positional, keyword);
    };
    for child in arg_list.children() {
        match child.kind() {
            SyntaxKind::PARAMETERS => {
                for kw in child.children() {
                    keyword.push(param_from_node(&kw));
                }
            }
            _ => positional.push(param_from_node(&child)),
        }
    }
    (positional, keyword)
}

/// Lower one argument node to a [`Param`], unwrapping an `ARG` wrapper. A
/// defaulted parameter is a `KEYWORD_ARG` or `ASSIGNMENT_EXPR` of
/// `pattern = default`; everything else is a plain pattern.
fn param_from_node(node: &SyntaxNode) -> Param {
    let inner = if node.kind() == SyntaxKind::ARG {
        node.children().next()
    } else {
        Some(node.clone())
    };
    let Some(inner) = inner else {
        return Param::default();
    };
    match inner.kind() {
        SyntaxKind::KEYWORD_ARG | SyntaxKind::ASSIGNMENT_EXPR => {
            let mut children = inner.children();
            let mut param = children
                .next()
                .map(|p| param_from_pattern(&p))
                .unwrap_or_default();
            param.default = children.next().map(|v| normalized_text(&v));
            param
        }
        _ => param_from_pattern(&inner),
    }
}

/// Lower a parameter pattern to a [`Param`], unwrapping `::T` annotations and
/// `x...` splats.
fn param_from_pattern(node: &SyntaxNode) -> Param {
    match node.kind() {
        SyntaxKind::NAME | SyntaxKind::NONSTANDARD_IDENTIFIER => Param {
            name: node_name_text(node),
            ..Param::default()
        },
        SyntaxKind::TYPE_ANNOTATION => {
            let (pattern, types) = annotation_parts(node);
            let mut param = pattern.as_ref().map(param_from_pattern).unwrap_or_default();
            param.type_annotation = types.first().map(lower_type);
            param
        }
        SyntaxKind::SPLAT_EXPR => {
            let mut param = node
                .children()
                .next()
                .map(|inner| param_from_pattern(&inner))
                .unwrap_or_default();
            param.is_vararg = true;
            param
        }
        _ => Param::default(),
    }
}

/// The name and its bounds structure for a type header: `Foo`, `Foo{T}`,
/// `Foo{T} <: Super`.
struct Header {
    name: Option<(String, TextRange)>,
    type_params: Vec<TypeExpr>,
    supertype: Option<TypeExpr>,
}

fn header_parts(start: &SyntaxNode) -> Header {
    let mut node = start.clone();
    let mut supertype = None;

    // Peel a `<: Super` layer, keeping the left as the name-carrying part.
    if matches!(
        node.kind(),
        SyntaxKind::BINARY_EXPR | SyntaxKind::COMPARISON_EXPR
    ) && has_subtype(&node)
    {
        let mut children = node.children();
        if let Some(name_part) = children.next() {
            if let Some(sup) = children.next() {
                supertype = Some(lower_type(&sup));
            }
            node = name_part;
        }
    }

    let (name, type_params) = if node.kind() == SyntaxKind::CURLY_EXPR {
        let base = node.children().next();
        let name = base
            .as_ref()
            .and_then(type_name_of)
            .and_then(|n| node_name_text(&n).map(|t| (t, n.text_range())));
        let params = node
            .children()
            .filter(|c| c.kind() == SyntaxKind::ARG_LIST)
            .flat_map(|list| list.children().collect::<Vec<_>>())
            .collect::<Vec<_>>();
        (name, lower_type_params(params.iter()))
    } else {
        let name =
            type_name_of(&node).and_then(|n| node_name_text(&n).map(|t| (t, n.text_range())));
        (name, Vec::new())
    };

    Header {
        name,
        type_params,
        supertype,
    }
}

fn push_annotated_field(node: &SyntaxNode, default: Option<String>, fields: &mut Vec<Field>) {
    let (pattern, types) = annotation_parts(node);
    let Some(name) = pattern.as_ref().and_then(node_name_text) else {
        return;
    };
    fields.push(Field {
        name,
        type_annotation: types.first().map(lower_type),
        default,
    });
}

/// Collect the `(name, range)` of every `NAME` a declaration target binds
/// (`const a, b`, `const x::Int`).
fn collect_names(node: &SyntaxNode) -> Vec<(String, TextRange)> {
    let mut out = Vec::new();
    collect_names_into(node, &mut out);
    out
}

fn collect_names_into(node: &SyntaxNode, out: &mut Vec<(String, TextRange)>) {
    match node.kind() {
        SyntaxKind::NAME | SyntaxKind::NONSTANDARD_IDENTIFIER => {
            if let Some(name) = node_name_text(node) {
                out.push((name, node.text_range()));
            }
        }
        SyntaxKind::TUPLE_EXPR
        | SyntaxKind::BARE_TUPLE_EXPR
        | SyntaxKind::ARG
        | SyntaxKind::SPLAT_EXPR
        | SyntaxKind::PAREN_EXPR => {
            for child in node.children() {
                collect_names_into(&child, out);
            }
        }
        SyntaxKind::TYPE_ANNOTATION => {
            if let Some(pattern) = annotation_parts(node).0 {
                collect_names_into(&pattern, out);
            }
        }
        _ => {}
    }
}

/// File `method` under its `(owner, name)` group in `dest`, creating the group
/// if absent and promoting the method's docstring to a group without one.
fn push_method(dest: &mut ModuleIndex, name: String, owner: Option<Vec<String>>, method: Method) {
    if let Some(group) = dest
        .functions
        .iter_mut()
        .find(|g| g.name == name && g.owner == owner)
    {
        if group.doc.is_none() {
            group.doc = method.doc.clone();
        }
        group.methods.push(method);
    } else {
        dest.functions.push(FunctionGroup {
            name,
            owner,
            doc: method.doc.clone(),
            methods: vec![method],
        });
    }
}

/// The final component of a `MACRO_NAME` inside a macro call (`@doc` → `doc`,
/// `Base.@kwdef` → `kwdef`).
fn macro_call_name(node: &SyntaxNode) -> Option<String> {
    let macro_name = node
        .children()
        .find(|c| c.kind() == SyntaxKind::MACRO_NAME)?;
    last_ident(&macro_name).map(|t| t.text().to_string())
}

/// The non-name argument nodes of a macro call, unwrapping an `ARG_LIST`/`ARG`.
fn macro_args(node: &SyntaxNode) -> Vec<SyntaxNode> {
    let mut out = Vec::new();
    for child in node.children() {
        match child.kind() {
            SyntaxKind::MACRO_NAME => {}
            SyntaxKind::ARG_LIST => {
                for arg in child.children() {
                    if arg.kind() == SyntaxKind::ARG {
                        out.extend(arg.children());
                    } else {
                        out.push(arg);
                    }
                }
            }
            _ => out.push(child),
        }
    }
    out
}

/// Whether a call's callee is the bare name `include`.
fn is_include_callee(call: &CallExpr) -> bool {
    matches!(call.callee(), Some(Expr::Name(name))
        if name.ident().map(|i| i.text() == "include").unwrap_or(false))
}

/// The name and its range for a `module`/`baremodule` definition.
fn module_name(node: &SyntaxNode) -> Option<(String, TextRange)> {
    let start = signature_start(node)?;
    let name = type_name_of(&start)?;
    Some((node_name_text(&name)?, name.text_range()))
}

/// The trailing size expression of a `primitive type T bits`, normalized.
fn primitive_bits(node: &SyntaxNode) -> Option<String> {
    // The signature holds the name; the bit-count is the last child expression.
    let sig = node
        .children()
        .find(|c| c.kind() == SyntaxKind::SIGNATURE)?;
    node.children()
        .filter(|c| c.text_range().start() >= sig.text_range().end() && is_expr(c.kind()))
        .last()
        .map(|c| normalized_text(&c))
}

/// The display text of a `NAME` (its ident) or a `var"..."` identifier (its
/// quoted content).
fn node_name_text(node: &SyntaxNode) -> Option<String> {
    match node.kind() {
        SyntaxKind::NAME => node
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT)
            .map(|t| t.text().to_string()),
        SyntaxKind::NONSTANDARD_IDENTIFIER => node
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::STRING_CONTENT)
            .map(|t| t.text().to_string()),
        _ => None,
    }
}

/// The last `IDENT` token of a node (a macro name's simple component).
fn last_ident(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::IDENT)
        .last()
}

fn has_token(node: &SyntaxNode, kind: SyntaxKind) -> bool {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == kind)
}

fn has_subtype(node: &SyntaxNode) -> bool {
    has_token(node, SyntaxKind::SUBTYPE)
}

fn is_trivia(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::WHITESPACE
            | SyntaxKind::NEWLINE
            | SyntaxKind::COMMENT
            | SyntaxKind::BLOCK_COMMENT
    )
}

fn is_expr(kind: SyntaxKind) -> bool {
    crate::ast::is_expr_kind(kind)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walk `text` as the top level of a package named `name`, returning the
    /// root module — the filesystem-free path into the harvester.
    fn harvest_str(text: &str, name: &str) -> ModuleIndex {
        let mut harvester = Harvester {
            root: PathBuf::from("/pkg"),
            visited: HashSet::new(),
            members: Vec::new(),
            member_modules: BTreeMap::new(),
            module_path: Vec::new(),
            diagnostics: Vec::new(),
            root_filled: false,
        };
        let mut root = ModuleIndex {
            name: name.to_string(),
            bare: false,
            loc: DefLocation {
                file: PathBuf::from("src/Pkg.jl"),
                range: Span { start: 0, end: 0 },
            },
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            consts: Vec::new(),
            macros: Vec::new(),
            submodules: Vec::new(),
        };
        harvester.walk_text(text, Path::new("/pkg/src/Pkg.jl"), &mut root, true);
        root
    }

    fn name_of(t: &Option<TypeExpr>) -> Option<Vec<&str>> {
        match t {
            Some(TypeExpr::Name { path }) => Some(path.iter().map(String::as_str).collect()),
            _ => None,
        }
    }

    #[test]
    fn positional_params_and_default() {
        let m = harvest_str("f(x::Int, y = 2) = x", "Pkg");
        assert_eq!(m.functions.len(), 1);
        let group = &m.functions[0];
        assert_eq!(group.name, "f");
        assert_eq!(group.owner, None);
        assert_eq!(group.methods.len(), 1);
        let method = &group.methods[0];
        assert_eq!(method.params.len(), 2);
        assert_eq!(method.params[0].name.as_deref(), Some("x"));
        assert_eq!(
            name_of(&method.params[0].type_annotation),
            Some(vec!["Int"])
        );
        assert_eq!(method.params[1].name.as_deref(), Some("y"));
        assert_eq!(method.params[1].default.as_deref(), Some("2"));
        assert!(method.has_body);
    }

    #[test]
    fn keyword_params_and_where() {
        let m = harvest_str(
            "function f(x; y::T = 1, kw...) where {T <: Real}\nend",
            "Pkg",
        );
        let method = &m.functions[0].methods[0];
        assert_eq!(method.params.len(), 1, "x is positional");
        assert_eq!(method.keyword_params.len(), 2);
        assert_eq!(method.keyword_params[0].name.as_deref(), Some("y"));
        assert_eq!(method.keyword_params[0].default.as_deref(), Some("1"));
        assert!(method.keyword_params[1].is_vararg, "kw... is a kw-splat");
        assert_eq!(method.where_clauses.len(), 1);
        assert!(matches!(
            &method.where_clauses[0],
            TypeExpr::TypeVar { name, upper: Some(_), .. } if name == "T"
        ));
    }

    #[test]
    fn return_type() {
        let m = harvest_str("g()::Float64 = 1.0", "Pkg");
        assert_eq!(
            name_of(&m.functions[0].methods[0].return_type),
            Some(vec!["Float64"])
        );
    }

    #[test]
    fn multiple_dispatch_groups_by_name() {
        let m = harvest_str("f(x::Int) = 1\nf(x::Float64) = 2", "Pkg");
        assert_eq!(m.functions.len(), 1, "one group");
        assert_eq!(m.functions[0].methods.len(), 2, "two methods");
    }

    #[test]
    fn qualified_extension_records_owner() {
        let m = harvest_str("function Base.show(io, x)\nend", "Pkg");
        let group = &m.functions[0];
        assert_eq!(group.name, "show");
        assert_eq!(group.owner, Some(vec!["Base".to_string()]));
    }

    #[test]
    fn operator_definition() {
        let m = harvest_str("+(a, b) = a", "Pkg");
        assert_eq!(m.functions[0].name, "+");
    }

    #[test]
    fn bodyless_function() {
        let m = harvest_str("function f end", "Pkg");
        let method = &m.functions[0].methods[0];
        assert!(!method.has_body);
        assert!(method.params.is_empty());
    }

    #[test]
    fn nonstandard_identifier_name() {
        let m = harvest_str("var\"my name\"() = 1", "Pkg");
        assert_eq!(m.functions[0].name, "my name");
    }

    #[test]
    fn mutable_struct_fields_supertype_params() {
        let m = harvest_str(
            "mutable struct Point{T} <: AbstractPoint\n    x::T\n    y::T = 0\nend",
            "Pkg",
        );
        assert_eq!(m.types.len(), 1);
        let ty = &m.types[0];
        assert_eq!(ty.name, "Point");
        assert_eq!(ty.kind, TypeKind::Struct { mutable: true });
        assert_eq!(name_of(&ty.supertype), Some(vec!["AbstractPoint"]));
        assert_eq!(ty.type_params.len(), 1);
        assert!(matches!(&ty.type_params[0], TypeExpr::TypeVar { name, .. } if name == "T"));
        assert_eq!(ty.fields.len(), 2);
        assert_eq!(ty.fields[0].name, "x");
        assert_eq!(name_of(&ty.fields[0].type_annotation), Some(vec!["T"]));
        assert_eq!(ty.fields[1].name, "y");
        assert_eq!(ty.fields[1].default.as_deref(), Some("0"));
    }

    #[test]
    fn abstract_and_primitive_types() {
        let m = harvest_str(
            "abstract type Animal <: Any end\nprimitive type Bits8 8 end",
            "Pkg",
        );
        assert_eq!(m.types[0].kind, TypeKind::Abstract);
        assert_eq!(name_of(&m.types[0].supertype), Some(vec!["Any"]));
        assert_eq!(
            m.types[1].kind,
            TypeKind::Primitive {
                bits: Some("8".to_string())
            }
        );
    }

    #[test]
    fn inner_constructor_files_under_type_name() {
        let m = harvest_str("struct Foo\n    x::Int\n    Foo() = new(0)\nend", "Pkg");
        assert_eq!(m.types[0].fields.len(), 1);
        let ctor = m.functions.iter().find(|g| g.name == "Foo");
        assert!(ctor.is_some(), "inner constructor filed as function Foo");
    }

    #[test]
    fn const_multiple_names() {
        let m = harvest_str("const a, b = 1, 2", "Pkg");
        let names: Vec<&str> = m.consts.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
    }

    #[test]
    fn const_single_value_preview() {
        let m = harvest_str("const K = 42", "Pkg");
        assert_eq!(m.consts[0].value_repr.as_deref(), Some("42"));
    }

    #[test]
    fn macro_definition_keeps_sigil() {
        let m = harvest_str("macro assert(cond)\nend", "Pkg");
        assert_eq!(m.macros[0].name, "@assert");
        assert_eq!(m.macros[0].params[0].name.as_deref(), Some("cond"));
    }

    #[test]
    fn exports_and_public() {
        let m = harvest_str("export a, +, @m\npublic x", "Pkg");
        let exported: Vec<&str> = m
            .exports
            .iter()
            .filter(|e| e.visibility == Visibility::Exported)
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(exported, ["a", "+", "@m"]);
        let public: Vec<&str> = m
            .exports
            .iter()
            .filter(|e| e.visibility == Visibility::Public)
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(public, ["x"], "the `public` keyword itself is skipped");
    }

    #[test]
    fn docstring_string_form() {
        let m = harvest_str("\"Adds one.\"\nf(x) = x + 1", "Pkg");
        let doc = m.functions[0].methods[0].doc.as_ref().unwrap();
        assert_eq!(doc.text, "Adds one.");
    }

    #[test]
    fn docstring_at_doc_form() {
        // The `@doc "..." <def>` form: the doc attaches to the inline
        // definition it wraps.
        let m = harvest_str("@doc \"Docs.\" f(x) = x", "Pkg");
        let group = m.functions.iter().find(|g| g.name == "f").unwrap();
        assert_eq!(group.methods[0].doc.as_ref().unwrap().text, "Docs.");
    }

    #[test]
    fn nested_module() {
        let m = harvest_str("module Sub\n    g() = 1\nend", "Pkg");
        assert_eq!(m.submodules.len(), 1);
        assert_eq!(m.submodules[0].name, "Sub");
        assert_eq!(m.submodules[0].functions[0].name, "g");
    }

    #[test]
    fn outer_module_absorbs_into_root() {
        let m = harvest_str("module Pkg\n    f() = 1\nend", "Pkg");
        assert!(m.submodules.is_empty(), "same-name module is the root");
        assert_eq!(m.functions[0].name, "f");
    }

    #[test]
    fn kwdef_struct_defaults() {
        let m = harvest_str(
            "@kwdef struct Config\n    verbose::Bool = false\n    level::Int = 1\nend",
            "Pkg",
        );
        assert_eq!(m.types[0].name, "Config");
        assert_eq!(m.types[0].fields.len(), 2);
        assert_eq!(m.types[0].fields[0].default.as_deref(), Some("false"));
    }

    #[test]
    fn transparent_macro_wrapper() {
        // `@inline` wraps a real definition we still want to harvest.
        let m = harvest_str("@inline f(x) = x", "Pkg");
        assert_eq!(m.functions[0].name, "f");
    }

    #[test]
    fn plain_global_is_skipped() {
        let m = harvest_str("x = 1", "Pkg");
        assert!(m.functions.is_empty());
        assert!(m.consts.is_empty());
    }
}
