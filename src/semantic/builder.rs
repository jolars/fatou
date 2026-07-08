//! The CST walk that populates a [`SemanticModel`].
//!
//! Two phases per scope, mirroring Julia's hoisting rule (any assignment in a
//! scope makes the name local to the *whole* scope, regardless of textual
//! position): `declare` shallowly scans a scope's extent — stopping at nested
//! scope boundaries — and introduces bindings; `walk` then records reads and
//! writes, descending into nested scopes, each of which runs its own declare
//! phase first. Reads therefore always resolve against fully populated
//! enclosing scopes, which makes forward closure captures come out right.

use rowan::TextRange;
use smol_str::SmolStr;

use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

use super::binding::{Binding, BindingId, BindingKind};
use super::import::{
    ExportEntry, ImportItem, LoadKind, ModuleLoad, ModulePath, QualifiedRead, Visibility,
};
use super::scope::{Scope, ScopeId, ScopeKind};
use super::{Access, IdentRef, SemanticModel};

pub(crate) fn build(root: &SyntaxNode) -> SemanticModel {
    let mut builder = Builder {
        model: SemanticModel::default(),
        global_decls: Vec::new(),
    };
    let file = builder.push_scope(ScopeKind::File, None, root.text_range());
    builder.declare_in(root, file);
    builder.walk_children(root, file);
    builder
        .model
        .idents
        .sort_by_key(|ident| (ident.range.start(), ident.range.end()));
    builder.model
}

/// Nodes that open a scope of their own (or are opaque, like quotes): the
/// declare phase does not look inside them, and the walk phase gives each a
/// dedicated handler as coverage grows.
fn creates_scope(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::FUNCTION_DEF
            | SyntaxKind::MACRO_DEF
            | SyntaxKind::ARROW_EXPR
            | SyntaxKind::DO_EXPR
            | SyntaxKind::LET_EXPR
            | SyntaxKind::FOR_EXPR
            | SyntaxKind::WHILE_EXPR
            | SyntaxKind::TRY_EXPR
            | SyntaxKind::COMPREHENSION
            | SyntaxKind::BRACES_COMPREHENSION
            | SyntaxKind::TYPED_COMPREHENSION
            | SyntaxKind::GENERATOR
            | SyntaxKind::STRUCT_DEF
            | SyntaxKind::ABSTRACT_DEF
            | SyntaxKind::PRIMITIVE_DEF
            | SyntaxKind::MODULE_DEF
            | SyntaxKind::QUOTE_EXPR
            | SyntaxKind::QUOTE_SYM
    )
}

fn is_augmented_assign(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::PLUS_EQ
            | SyntaxKind::MINUS_EQ
            | SyntaxKind::STAR_EQ
            | SyntaxKind::SLASH_EQ
            | SyntaxKind::BACKSLASH_EQ
            | SyntaxKind::SLASH_SLASH_EQ
            | SyntaxKind::CARET_EQ
            | SyntaxKind::PERCENT_EQ
            | SyntaxKind::PIPE_EQ
            | SyntaxKind::AMP_EQ
            | SyntaxKind::SHL_EQ
            | SyntaxKind::SHR_EQ
            | SyntaxKind::USHR_EQ
            | SyntaxKind::DIV_EQ
            | SyntaxKind::XOR_EQ
    )
}

/// Broadcast assignment (`.=`, `.+=`, ...) mutates elements in place: the
/// target is an ordinary read, never a binding.
fn is_broadcast_assign(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::DOT_EQ
            | SyntaxKind::DOT_PLUS_EQ
            | SyntaxKind::DOT_MINUS_EQ
            | SyntaxKind::DOT_STAR_EQ
            | SyntaxKind::DOT_SLASH_EQ
            | SyntaxKind::DOT_BACKSLASH_EQ
            | SyntaxKind::DOT_SLASH_SLASH_EQ
            | SyntaxKind::DOT_CARET_EQ
            | SyntaxKind::DOT_PERCENT_EQ
            | SyntaxKind::DOT_SHL_EQ
            | SyntaxKind::DOT_SHR_EQ
            | SyntaxKind::DOT_USHR_EQ
            | SyntaxKind::DOT_DIV_EQ
            | SyntaxKind::DOT_XOR_EQ
    )
}

/// The assignment operator of an `ASSIGNMENT_EXPR`, classified.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AssignOp {
    Plain,
    Augmented,
    Broadcast,
}

fn assign_op(node: &SyntaxNode) -> AssignOp {
    for element in node.children_with_tokens() {
        if let Some(token) = element.into_token() {
            let kind = token.kind();
            if is_augmented_assign(kind) {
                return AssignOp::Augmented;
            }
            if is_broadcast_assign(kind) {
                return AssignOp::Broadcast;
            }
            if matches!(kind, SyntaxKind::EQ | SyntaxKind::UNICODE_ASSIGN_OP) {
                return AssignOp::Plain;
            }
        }
    }
    AssignOp::Plain
}

/// `a.b` field access: a `BINARY_EXPR` whose operator is a plain `.`. Only
/// the leftmost operand is a variable read; the field name is data.
fn is_field_access(node: &SyntaxNode) -> bool {
    node.kind() == SyntaxKind::BINARY_EXPR
        && node
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .any(|t| t.kind() == SyntaxKind::DOT)
}

fn name_ident(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == SyntaxKind::IDENT)
}

/// One `using`/`import` clause (an `IMPORT_PATH`, or the path inside an
/// `IMPORT_ALIAS`), extracted from the raw token stream the parser leaves in
/// the tree. Mirrors the sexpr projector's reading of the same shape.
struct ImportClause {
    leading_dots: u32,
    components: Vec<(SmolStr, TextRange)>,
    alias: Option<(SmolStr, TextRange)>,
    range: TextRange,
    /// The clause ends in an interpolation (`import $A`, `import A.$B`), so
    /// the name it binds is unknowable.
    unbindable: bool,
}

impl ImportClause {
    /// The name this clause binds (the alias, else the last component), or
    /// `None` when interpolation makes it unknowable.
    fn binding_name(&self) -> Option<(&SmolStr, TextRange)> {
        if let Some((name, range)) = &self.alias {
            return Some((name, *range));
        }
        if self.unbindable {
            return None;
        }
        self.components.last().map(|(name, range)| (name, *range))
    }

    fn path(&self) -> ModulePath {
        ModulePath {
            leading_dots: self.leading_dots,
            components: self
                .components
                .iter()
                .map(|(name, _)| name.clone())
                .collect(),
        }
    }

    /// The clause as an explicit import item (`using X: a as b`).
    fn as_item(&self) -> Option<ImportItem> {
        if self.unbindable || self.components.is_empty() {
            return None;
        }
        Some(ImportItem {
            name: self.components.last().unwrap().0.clone(),
            alias: self.alias.as_ref().map(|(name, _)| name.clone()),
            range: self.range,
        })
    }
}

/// Read the dot-separated components out of an `IMPORT_PATH` node's tokens:
/// leading `.`/`..`/`...` count as relative dots, `IDENT` and operator
/// tokens are components (fused dotted operators like `.==` contribute a
/// relative dot when leading), quoted operators (`A.:+`, `A.(:+)`) unwrap to
/// the quoted symbol, macro names keep their `@`, and interpolations mark
/// the clause unbindable.
fn import_path_parts(path: &SyntaxNode, clause: &mut ImportClause) {
    let mut seen_name = false;
    let component = |clause: &mut ImportClause, text: &str, range: TextRange| {
        clause.components.push((SmolStr::new(text), range));
        clause.unbindable = false;
    };
    for element in path.children_with_tokens() {
        match element {
            rowan::NodeOrToken::Token(t) => match t.kind() {
                SyntaxKind::DOT if !seen_name => clause.leading_dots += 1,
                SyntaxKind::DOT_DOT if !seen_name => clause.leading_dots += 2,
                SyntaxKind::DOT_DOT_DOT if !seen_name => clause.leading_dots += 3,
                SyntaxKind::IDENT => {
                    component(clause, t.text(), t.text_range());
                    seen_name = true;
                }
                // After a name, `...` is a separator dot fused with the `..`
                // range operator as a component (`import A...`).
                SyntaxKind::DOT_DOT_DOT => component(clause, "..", t.text_range()),
                SyntaxKind::DOT | SyntaxKind::DOT_DOT | SyntaxKind::COLON => {}
                k if k.is_operator() => {
                    // A fused dotted operator's leading dot is a relative
                    // dot before any name (`import .==`), a separator after.
                    if !seen_name && t.text().starts_with('.') {
                        clause.leading_dots += 1;
                    }
                    component(clause, t.text().trim_start_matches('.'), t.text_range());
                    seen_name = true;
                }
                _ => {}
            },
            rowan::NodeOrToken::Node(n) => match n.kind() {
                SyntaxKind::NAME => {
                    if let Some(t) = name_ident(&n) {
                        component(clause, t.text(), t.text_range());
                    }
                    seen_name = true;
                }
                // `A.:+` and `A.(:+)`: the component is the quoted symbol.
                SyntaxKind::QUOTE_SYM | SyntaxKind::PAREN_EXPR => {
                    if let Some(t) = quoted_symbol_token(&n) {
                        component(clause, t.text(), t.text_range());
                    }
                    seen_name = true;
                }
                SyntaxKind::MACRO_NAME => {
                    if let Some(t) = macro_name_ident(&n) {
                        component(clause, &format!("@{}", t.text()), n.text_range());
                    }
                    seen_name = true;
                }
                SyntaxKind::INTERPOLATION => {
                    clause.unbindable = true;
                    seen_name = true;
                }
                _ => {}
            },
        }
    }
}

/// The symbol token inside a quoted-operator path component (`:+`, `:(+)`,
/// `:(foo)`): the first identifier or operator token after the quote colon.
fn quoted_symbol_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| {
            t.kind() == SyntaxKind::IDENT
                || (t.kind().is_operator() && t.kind() != SyntaxKind::COLON)
        })
}

/// The final identifier token of a `MACRO_NAME` (the name after the `@`).
fn macro_name_ident(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| t.kind() == SyntaxKind::IDENT)
        .last()
}

/// Split a `USING_STMT`/`IMPORT_STMT` into the clauses before an optional
/// colon (the comma-form paths, or the item list's single base path) and the
/// item clauses after it. ERROR-wrapped clauses (invalid `as` positions) are
/// skipped. A statement-level `$` (the parser leaves `import A.$B` partly
/// outside the path node) poisons the preceding clause.
fn collect_import_clauses(stmt: &SyntaxNode) -> (Vec<ImportClause>, Option<Vec<ImportClause>>) {
    let mut before: Vec<ImportClause> = Vec::new();
    let mut after: Option<Vec<ImportClause>> = None;
    for element in stmt.children_with_tokens() {
        match element {
            rowan::NodeOrToken::Token(t) => match t.kind() {
                SyntaxKind::COLON => after = Some(Vec::new()),
                SyntaxKind::DOLLAR => {
                    if let Some(clause) = after.as_mut().unwrap_or(&mut before).last_mut() {
                        clause.unbindable = true;
                    }
                }
                _ => {}
            },
            rowan::NodeOrToken::Node(n)
                if matches!(n.kind(), SyntaxKind::IMPORT_PATH | SyntaxKind::IMPORT_ALIAS) =>
            {
                let mut clause = ImportClause {
                    leading_dots: 0,
                    components: Vec::new(),
                    alias: None,
                    range: n.text_range(),
                    unbindable: false,
                };
                let path = if n.kind() == SyntaxKind::IMPORT_ALIAS {
                    // `IMPORT_PATH … as alias`: the alias is the last bare
                    // identifier token (the path's own names are nested).
                    clause.alias = n
                        .children_with_tokens()
                        .filter_map(|e| e.into_token())
                        .filter(|t| t.kind() == SyntaxKind::IDENT && t.text() != "as")
                        .last()
                        .map(|t| (SmolStr::new(t.text()), t.text_range()));
                    n.children().find(|c| c.kind() == SyntaxKind::IMPORT_PATH)
                } else {
                    Some(n.clone())
                };
                if let Some(path) = path {
                    import_path_parts(&path, &mut clause);
                }
                after.as_mut().unwrap_or(&mut before).push(clause);
            }
            _ => {}
        }
    }
    (before, after)
}

/// Flatten a pure dotted-name chain (`a.b.c`) into its components plus the
/// root `NAME` node; `None` when any part is not a plain name (`f(x).y`).
fn qualified_name_chain(node: &SyntaxNode) -> Option<(Vec<SmolStr>, SyntaxNode)> {
    let mut reversed: Vec<SmolStr> = Vec::new();
    let mut cursor = node.clone();
    loop {
        let mut children = cursor.children();
        let lhs = children.next()?;
        let rhs = children.next()?;
        let field = name_ident(&rhs).filter(|_| rhs.kind() == SyntaxKind::NAME)?;
        reversed.push(SmolStr::new(field.text()));
        match lhs.kind() {
            SyntaxKind::NAME => {
                reversed.push(SmolStr::new(name_ident(&lhs)?.text()));
                reversed.reverse();
                return Some((reversed, lhs));
            }
            SyntaxKind::BINARY_EXPR if is_field_access(&lhs) => cursor = lhs,
            _ => return None,
        }
    }
}

/// Split a `TYPE_ANNOTATION` into the annotated pattern (absent for the
/// unnamed-argument form `::Int`, where `::` precedes the only child) and
/// the type nodes after the `::`.
fn annotation_parts(node: &SyntaxNode) -> (Option<SyntaxNode>, Vec<SyntaxNode>) {
    let mut pattern = None;
    let mut types = Vec::new();
    let mut seen_colon = false;
    for element in node.children_with_tokens() {
        match element {
            rowan::NodeOrToken::Token(token) if token.kind() == SyntaxKind::COLON_COLON => {
                seen_colon = true;
            }
            rowan::NodeOrToken::Node(child) => {
                if seen_colon {
                    types.push(child);
                } else {
                    pattern = Some(child);
                }
            }
            _ => {}
        }
    }
    (pattern, types)
}

/// Peel `where` clauses and a return-type annotation off a signature
/// expression, down to the core (a `CALL_EXPR`, or a `TUPLE_EXPR`/`NAME`
/// for anonymous and bare forms). Returns the core, the `where` parameter
/// specs (outermost clause first), and the return type, if any.
fn peel_signature(start: SyntaxNode) -> (Option<SyntaxNode>, Vec<SyntaxNode>, Option<SyntaxNode>) {
    let mut wheres = Vec::new();
    let mut return_ty = None;
    let mut cursor = Some(start);
    while let Some(node) = cursor {
        match node.kind() {
            SyntaxKind::WHERE_EXPR => {
                let mut children = node.children();
                cursor = children.next();
                wheres.extend(children);
            }
            SyntaxKind::TYPE_ANNOTATION => {
                let (pattern, types) = annotation_parts(&node);
                // Only a call can carry a return type; `x::Int` is not a
                // signature layer.
                if pattern.as_ref().is_some_and(has_call_core) {
                    return_ty = types.into_iter().next();
                    cursor = pattern;
                } else {
                    return (Some(node), wheres, return_ty);
                }
            }
            _ => return (Some(node), wheres, return_ty),
        }
    }
    (None, wheres, return_ty)
}

/// Whether peeling `node` bottoms out at a `CALL_EXPR` (a function
/// signature rather than a plain assignment target).
fn has_call_core(node: &SyntaxNode) -> bool {
    let mut cursor = Some(node.clone());
    while let Some(n) = cursor {
        match n.kind() {
            SyntaxKind::CALL_EXPR => return true,
            SyntaxKind::WHERE_EXPR => cursor = n.children().next(),
            SyntaxKind::TYPE_ANNOTATION => cursor = annotation_parts(&n).0,
            _ => return false,
        }
    }
    false
}

/// Where an assignment to a name lands.
enum AssignSlot {
    Existing(BindingId),
    NewIn(ScopeId, BindingKind),
}

/// Which declaration statement a name comes from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DeclKind {
    Local,
    Global,
    Const,
}

/// The `NAME` a type-definition signature introduces: peels `Foo{T} <: Super`
/// layers down to `Foo`.
fn type_name_of(start: &SyntaxNode) -> Option<SyntaxNode> {
    match start.kind() {
        SyntaxKind::NAME => Some(start.clone()),
        SyntaxKind::CURLY_EXPR | SyntaxKind::BINARY_EXPR | SyntaxKind::COMPARISON_EXPR => {
            start.children().next().and_then(|c| type_name_of(&c))
        }
        _ => None,
    }
}

struct Builder {
    model: SemanticModel,
    /// Names declared `global` per scope (builder-transient, parallel to
    /// `model.scopes`).
    global_decls: Vec<Vec<SmolStr>>,
}

impl Builder {
    fn push_scope(
        &mut self,
        kind: ScopeKind,
        parent: Option<ScopeId>,
        range: TextRange,
    ) -> ScopeId {
        let id = ScopeId(self.model.scopes.len() as u32);
        self.model.scopes.push(Scope {
            kind,
            parent,
            range,
            bindings: Vec::new(),
        });
        self.global_decls.push(Vec::new());
        id
    }

    fn push_binding(
        &mut self,
        name: &str,
        kind: BindingKind,
        scope: ScopeId,
        def_range: TextRange,
    ) -> BindingId {
        let id = BindingId(self.model.bindings.len() as u32);
        self.model.bindings.push(Binding {
            name: SmolStr::new(name),
            kind,
            scope,
            def_range,
            read: false,
        });
        self.model.scopes[scope.0 as usize].bindings.push(id);
        id
    }

    fn push_ident(
        &mut self,
        name: &str,
        range: TextRange,
        scope: ScopeId,
        access: Access,
        is_macro: bool,
        binding: Option<BindingId>,
    ) {
        self.model.idents.push(IdentRef {
            name: SmolStr::new(name),
            range,
            scope,
            access,
            is_macro,
            binding,
        });
    }

    fn scope(&self, id: ScopeId) -> &Scope {
        &self.model.scopes[id.0 as usize]
    }

    fn find_in_scope(&self, scope: ScopeId, name: &str) -> Option<BindingId> {
        self.scope(scope)
            .bindings
            .iter()
            .rev()
            .copied()
            .find(|&b| self.model.bindings[b.0 as usize].name == name)
    }

    /// The innermost global scope enclosing (or equal to) `scope`.
    fn innermost_global(&self, scope: ScopeId) -> ScopeId {
        let mut cursor = scope;
        loop {
            let s = self.scope(cursor);
            if s.kind.is_global() {
                return cursor;
            }
            cursor = s.parent.expect("local scope chains end at a global scope");
        }
    }

    /// Julia's assignment-target rule. A `global` declaration up the local
    /// chain routes to the innermost global scope; an existing local
    /// anywhere up the local chain (closures included) is the target;
    /// otherwise the assignment introduces a binding in its own scope —
    /// local in a local scope, global in a global one.
    fn resolve_assign(&self, name: &str, scope: ScopeId) -> AssignSlot {
        if self.scope(scope).kind.is_global() {
            return match self.find_in_scope(scope, name) {
                Some(b) => AssignSlot::Existing(b),
                None => AssignSlot::NewIn(scope, BindingKind::Global),
            };
        }
        let mut cursor = scope;
        loop {
            let s = self.scope(cursor);
            if s.kind.is_global() {
                break;
            }
            if self.global_decls[cursor.0 as usize]
                .iter()
                .any(|n| n == name)
            {
                let global = self.innermost_global(cursor);
                return match self.find_in_scope(global, name) {
                    Some(b) => AssignSlot::Existing(b),
                    None => AssignSlot::NewIn(global, BindingKind::Global),
                };
            }
            if let Some(b) = self.find_in_scope(cursor, name) {
                return AssignSlot::Existing(b);
            }
            match s.parent {
                Some(parent) => cursor = parent,
                None => break,
            }
        }
        AssignSlot::NewIn(scope, BindingKind::Local)
    }

    /// Resolve a read up the scope chain; the first global scope is checked
    /// and then terminates the search (module bodies do not see enclosing
    /// globals).
    fn resolve_read(&self, name: &str, scope: ScopeId) -> Option<BindingId> {
        let mut cursor = Some(scope);
        while let Some(id) = cursor {
            if let Some(b) = self.find_in_scope(id, name) {
                return Some(b);
            }
            let s = self.scope(id);
            cursor = if s.kind.is_global() { None } else { s.parent };
        }
        None
    }

    /// Resolve a `@name` read: the macro namespace sees `macro` definitions
    /// and imported macros (whose bindings keep the `@` sigil in the name).
    fn resolve_macro_read(&self, name: &str, scope: ScopeId) -> Option<BindingId> {
        let mut cursor = Some(scope);
        while let Some(id) = cursor {
            let hit = self.scope(id).bindings.iter().rev().copied().find(|&b| {
                let binding = &self.model.bindings[b.0 as usize];
                match binding.kind {
                    BindingKind::Macro => binding.name == name,
                    BindingKind::Import => binding.name.strip_prefix('@') == Some(name),
                    _ => false,
                }
            });
            if hit.is_some() {
                return hit;
            }
            let s = self.scope(id);
            cursor = if s.kind.is_global() { None } else { s.parent };
        }
        None
    }

    // --- declare phase -----------------------------------------------------

    /// Introduce the bindings assigned anywhere in `scope`'s own extent,
    /// without descending into nested scopes.
    fn declare_in(&mut self, node: &SyntaxNode, scope: ScopeId) {
        for child in node.children() {
            self.declare_node(&child, scope);
        }
    }

    fn declare_node(&mut self, node: &SyntaxNode, scope: ScopeId) {
        match node.kind() {
            SyntaxKind::FUNCTION_DEF => {
                self.declare_function_name(node, scope, BindingKind::Function);
            }
            SyntaxKind::MACRO_DEF => {
                self.declare_function_name(node, scope, BindingKind::Macro);
            }
            SyntaxKind::STRUCT_DEF | SyntaxKind::ABSTRACT_DEF | SyntaxKind::PRIMITIVE_DEF => {
                self.declare_type_name(node, scope);
            }
            SyntaxKind::MODULE_DEF => self.declare_module_name(node, scope),
            SyntaxKind::LOCAL_STMT => self.declare_declaration(node, scope, DeclKind::Local),
            SyntaxKind::GLOBAL_STMT => self.declare_declaration(node, scope, DeclKind::Global),
            SyntaxKind::CONST_STMT => self.declare_declaration(node, scope, DeclKind::Const),
            SyntaxKind::IMPORT_STMT | SyntaxKind::USING_STMT => {
                self.declare_import(node, scope);
            }
            kind if creates_scope(kind) => {}
            SyntaxKind::ASSIGNMENT_EXPR => {
                let mut children = node.children();
                let lhs = children.next();
                if let Some(lhs) = &lhs
                    && assign_op(node) == AssignOp::Plain
                    && has_call_core(lhs)
                {
                    // Short-form `f(x) = ...`: bind the name; the right-hand
                    // side is the function body, a nested scope.
                    self.declare_signature_name(lhs.clone(), scope, BindingKind::Function);
                    return;
                }
                if let Some(lhs) = lhs
                    && assign_op(node) != AssignOp::Broadcast
                {
                    self.declare_target(&lhs, scope);
                }
                // Nested assignments hide in the value (`a = b = 1`) and in
                // non-binding target positions (`x[i] = ...` indices).
                if let Some(lhs) = node.children().next()
                    && !is_binding_target(lhs.kind())
                {
                    self.declare_node(&lhs, scope);
                }
                for rest in children {
                    self.declare_node(&rest, scope);
                }
            }
            _ => self.declare_in(node, scope),
        }
    }

    /// Bind the names a `using`/`import` statement introduces: each comma
    /// clause's last path component (or its `as` alias), or the explicit
    /// items after a colon. Imported macros keep their `@` sigil, which
    /// keeps them out of value resolution (names never contain `@`).
    fn declare_import(&mut self, node: &SyntaxNode, scope: ScopeId) {
        let (before, after) = collect_import_clauses(node);
        let clauses = match &after {
            // An ERROR-wrapped base (`using A as B: x`) poisons the statement.
            Some(items) if !before.is_empty() => items,
            Some(_) => return,
            None => &before,
        };
        for clause in clauses {
            if let Some((name, range)) = clause.binding_name()
                && self.find_in_scope(scope, name).is_none()
            {
                self.push_binding(name, BindingKind::Import, scope, range);
            }
        }
    }

    /// Bind the name of a `function`/`macro` definition in its enclosing
    /// scope. Qualified names (`Base.foo`) are method extensions and bind
    /// nothing.
    fn declare_function_name(&mut self, node: &SyntaxNode, scope: ScopeId, kind: BindingKind) {
        let Some(sig) = node.children().find(|c| c.kind() == SyntaxKind::SIGNATURE) else {
            return;
        };
        if let Some(start) = sig.children().next() {
            self.declare_signature_name(start, scope, kind);
        }
    }

    fn declare_signature_name(&mut self, start: SyntaxNode, scope: ScopeId, kind: BindingKind) {
        let (core, _, _) = peel_signature(start);
        let name = match core {
            Some(core) if core.kind() == SyntaxKind::CALL_EXPR => core
                .children()
                .next()
                .filter(|c| c.kind() == SyntaxKind::NAME),
            Some(core) if core.kind() == SyntaxKind::NAME => Some(core),
            _ => None,
        };
        if let Some(name) = name
            && let Some(token) = name_ident(&name)
        {
            self.declare_name(&token, scope, Some(kind));
        }
    }

    /// Bind the name of a `struct`/`abstract type`/`primitive type` in its
    /// enclosing scope.
    fn declare_type_name(&mut self, node: &SyntaxNode, scope: ScopeId) {
        if let Some(name) = node
            .children()
            .find(|c| c.kind() == SyntaxKind::SIGNATURE)
            .and_then(|sig| sig.children().next())
            .and_then(|start| type_name_of(&start))
            && let Some(token) = name_ident(&name)
        {
            self.declare_name(&token, scope, Some(BindingKind::Type));
        }
    }

    fn declare_module_name(&mut self, node: &SyntaxNode, scope: ScopeId) {
        if let Some(name) = node
            .children()
            .find(|c| c.kind() == SyntaxKind::SIGNATURE)
            .and_then(|sig| sig.children().find(|c| c.kind() == SyntaxKind::NAME))
            && let Some(token) = name_ident(&name)
        {
            self.declare_name(&token, scope, Some(BindingKind::Module));
        }
    }

    /// `local`/`global`/`const` statements, with bare-name and assignment
    /// payloads. `local` forces a binding in the current scope (shadowing
    /// any enclosing local); `global` routes the names to the innermost
    /// global scope and records the declaration so later assignments in
    /// this scope follow it.
    fn declare_declaration(&mut self, node: &SyntaxNode, scope: ScopeId, decl: DeclKind) {
        for child in node.children() {
            self.declare_decl_pattern(&child, scope, decl);
        }
    }

    fn declare_decl_pattern(&mut self, node: &SyntaxNode, scope: ScopeId, decl: DeclKind) {
        match node.kind() {
            SyntaxKind::NAME => {
                if let Some(token) = name_ident(node) {
                    self.declare_declared_name(&token, scope, decl);
                }
            }
            SyntaxKind::TUPLE_EXPR
            | SyntaxKind::BARE_TUPLE_EXPR
            | SyntaxKind::ARG
            | SyntaxKind::SPLAT_EXPR
            | SyntaxKind::PAREN_EXPR => {
                for child in node.children() {
                    self.declare_decl_pattern(&child, scope, decl);
                }
            }
            SyntaxKind::TYPE_ANNOTATION => {
                if let Some(pattern) = annotation_parts(node).0 {
                    self.declare_decl_pattern(&pattern, scope, decl);
                }
            }
            SyntaxKind::ASSIGNMENT_EXPR => {
                let mut children = node.children();
                if let Some(target) = children.next() {
                    self.declare_decl_pattern(&target, scope, decl);
                }
                for rest in children {
                    self.declare_node(&rest, scope);
                }
            }
            _ => self.declare_node(node, scope),
        }
    }

    fn declare_declared_name(&mut self, token: &SyntaxToken, scope: ScopeId, decl: DeclKind) {
        if token.text() == "_" {
            return;
        }
        match decl {
            DeclKind::Local | DeclKind::Const => {
                let kind = if decl == DeclKind::Const {
                    BindingKind::Const
                } else {
                    BindingKind::Local
                };
                if self.find_in_scope(scope, token.text()).is_none() {
                    self.push_binding(token.text(), kind, scope, token.text_range());
                }
            }
            DeclKind::Global => {
                self.global_decls[scope.0 as usize].push(SmolStr::new(token.text()));
                let global = self.innermost_global(scope);
                if self.find_in_scope(global, token.text()).is_none() {
                    self.push_binding(
                        token.text(),
                        BindingKind::Global,
                        global,
                        token.text_range(),
                    );
                }
            }
        }
    }

    fn declare_target(&mut self, node: &SyntaxNode, scope: ScopeId) {
        match node.kind() {
            SyntaxKind::NAME => {
                if let Some(token) = name_ident(node) {
                    self.declare_name(&token, scope, None);
                }
            }
            SyntaxKind::TUPLE_EXPR
            | SyntaxKind::BARE_TUPLE_EXPR
            | SyntaxKind::ARG
            | SyntaxKind::SPLAT_EXPR
            | SyntaxKind::PAREN_EXPR => {
                for child in node.children() {
                    self.declare_target(&child, scope);
                }
            }
            SyntaxKind::TYPE_ANNOTATION => {
                if let Some(first) = node.children().next() {
                    self.declare_target(&first, scope);
                }
            }
            // Index/field/call targets bind nothing.
            _ => {}
        }
    }

    /// Introduce a binding for an assigned name if the assignment rule says
    /// this is a fresh variable; otherwise the existing binding is the
    /// target and nothing changes. `kind` overrides the rule's default
    /// Local/Global classification (e.g. for function definitions).
    fn declare_name(&mut self, token: &SyntaxToken, scope: ScopeId, kind: Option<BindingKind>) {
        if token.text() == "_" {
            return;
        }
        if let AssignSlot::NewIn(target, default_kind) = self.resolve_assign(token.text(), scope) {
            self.push_binding(
                token.text(),
                kind.unwrap_or(default_kind),
                target,
                token.text_range(),
            );
        }
    }

    // --- walk phase --------------------------------------------------------

    fn walk_children(&mut self, node: &SyntaxNode, scope: ScopeId) {
        for child in node.children() {
            self.walk_node(&child, scope);
        }
    }

    fn walk_node(&mut self, node: &SyntaxNode, scope: ScopeId) {
        match node.kind() {
            SyntaxKind::NAME => self.record_name_read(node, scope),
            SyntaxKind::ASSIGNMENT_EXPR => self.handle_assignment(node, scope),
            SyntaxKind::FUNCTION_DEF => self.handle_function_def(node, scope),
            SyntaxKind::MACRO_DEF => self.handle_function_def(node, scope),
            SyntaxKind::ARROW_EXPR => self.handle_arrow(node, scope),
            SyntaxKind::DO_EXPR => self.handle_do(node, scope),
            SyntaxKind::LET_EXPR => self.handle_let(node, scope),
            SyntaxKind::FOR_EXPR => self.handle_for(node, scope),
            SyntaxKind::WHILE_EXPR => self.handle_while(node, scope),
            SyntaxKind::TRY_EXPR => self.handle_try(node, scope),
            SyntaxKind::COMPREHENSION
            | SyntaxKind::BRACES_COMPREHENSION
            | SyntaxKind::TYPED_COMPREHENSION
            | SyntaxKind::GENERATOR => self.handle_comprehension(node, scope),
            SyntaxKind::STRUCT_DEF | SyntaxKind::ABSTRACT_DEF | SyntaxKind::PRIMITIVE_DEF => {
                self.handle_type_def(node, scope);
            }
            SyntaxKind::MODULE_DEF => self.handle_module(node, scope),
            SyntaxKind::IMPORT_STMT | SyntaxKind::USING_STMT => self.handle_import(node, scope),
            SyntaxKind::EXPORT_STMT | SyntaxKind::PUBLIC_STMT => {
                self.handle_name_list(node, scope);
            }
            SyntaxKind::LOCAL_STMT | SyntaxKind::GLOBAL_STMT | SyntaxKind::CONST_STMT => {
                self.handle_declaration(node, scope);
            }
            SyntaxKind::BINARY_EXPR if is_field_access(node) => {
                // A pure dotted-name chain is a qualified read (`Foo.bar`),
                // recorded whole alongside the root's ordinary read; mixed
                // chains (`f(x).y`) just walk the base as before.
                if let Some((path, root)) = qualified_name_chain(node) {
                    self.model.qualified_reads.push(QualifiedRead {
                        path,
                        range: node.text_range(),
                        scope,
                        is_macro: false,
                    });
                    self.record_name_read(&root, scope);
                } else if let Some(base) = node.children().next() {
                    self.walk_node(&base, scope);
                }
            }
            SyntaxKind::KEYWORD_ARG => {
                // Call-site `f(x = v)`: the keyword name is not a variable.
                for child in node.children().skip(1) {
                    self.walk_node(&child, scope);
                }
            }
            SyntaxKind::INTERPOLATION => self.walk_interpolation(node, scope),
            SyntaxKind::MACRO_CALL => self.walk_macro_call(node, scope),
            SyntaxKind::QUOTE_EXPR | SyntaxKind::QUOTE_SYM => self.walk_quoted(node, scope),
            _ => self.walk_children(node, scope),
        }
    }

    fn handle_assignment(&mut self, node: &SyntaxNode, scope: ScopeId) {
        let op = assign_op(node);
        let mut children = node.children();
        let Some(lhs) = children.next() else { return };
        if op == AssignOp::Plain && has_call_core(&lhs) {
            return self.handle_short_form(node, scope, lhs);
        }
        if op == AssignOp::Broadcast {
            self.walk_node(&lhs, scope);
        } else {
            let access = if op == AssignOp::Augmented {
                Access::ReadWrite
            } else {
                Access::Write
            };
            self.walk_target(&lhs, scope, access);
        }
        for rest in children {
            self.walk_node(&rest, scope);
        }
    }

    fn walk_target(&mut self, node: &SyntaxNode, scope: ScopeId, access: Access) {
        match node.kind() {
            SyntaxKind::NAME => {
                if let Some(token) = name_ident(node) {
                    self.write_name(&token, scope, access);
                }
            }
            SyntaxKind::TUPLE_EXPR
            | SyntaxKind::BARE_TUPLE_EXPR
            | SyntaxKind::ARG
            | SyntaxKind::SPLAT_EXPR
            | SyntaxKind::PAREN_EXPR => {
                for child in node.children() {
                    self.walk_target(&child, scope, access);
                }
            }
            SyntaxKind::TYPE_ANNOTATION => {
                let mut children = node.children();
                if let Some(first) = children.next() {
                    self.walk_target(&first, scope, access);
                }
                for annotation in children {
                    self.walk_node(&annotation, scope);
                }
            }
            // `x[i] = v`, `x.f = v`, and anything else that mutates through
            // a value: plain reads.
            _ => self.walk_node(node, scope),
        }
    }

    fn write_name(&mut self, token: &SyntaxToken, scope: ScopeId, access: Access) {
        if token.text() == "_" {
            return;
        }
        let binding = match self.resolve_assign(token.text(), scope) {
            AssignSlot::Existing(b) => b,
            // The declare phase covers everything it scans; this arm picks
            // up targets in constructs it skips (nested scopes still being
            // grown handler by handler).
            AssignSlot::NewIn(target, kind) => {
                self.push_binding(token.text(), kind, target, token.text_range())
            }
        };
        if access == Access::ReadWrite {
            self.model.bindings[binding.0 as usize].read = true;
        }
        if self.model.bindings[binding.0 as usize].def_range == token.text_range() {
            return; // the definition site is not an occurrence of itself
        }
        self.push_ident(
            token.text(),
            token.text_range(),
            scope,
            access,
            false,
            Some(binding),
        );
    }

    fn record_name_read(&mut self, node: &SyntaxNode, scope: ScopeId) {
        if let Some(token) = name_ident(node) {
            self.record_token_read(&token, scope);
        }
    }

    fn record_token_read(&mut self, token: &SyntaxToken, scope: ScopeId) {
        if token.text() == "_" {
            return;
        }
        let binding = self.resolve_read(token.text(), scope);
        if let Some(b) = binding {
            self.model.bindings[b.0 as usize].read = true;
        }
        self.push_ident(
            token.text(),
            token.text_range(),
            scope,
            Access::Read,
            false,
            binding,
        );
    }

    // --- imports and exports -------------------------------------------------

    /// Record a `using`/`import` statement into the loaded-modules list: one
    /// entry per comma clause, or one entry carrying the item list of the
    /// colon form. The declare phase already introduced the bindings; here
    /// only interpolations are walked (they splice in enclosing values).
    fn handle_import(&mut self, node: &SyntaxNode, scope: ScopeId) {
        let kind = if node.kind() == SyntaxKind::USING_STMT {
            LoadKind::Using
        } else {
            LoadKind::Import
        };
        let (before, after) = collect_import_clauses(node);
        if let Some(items) = after {
            if let Some(base) = before.into_iter().next() {
                self.model.module_loads.push(ModuleLoad {
                    kind,
                    path: base.path(),
                    alias: base.alias.map(|(name, _)| name),
                    items: Some(items.iter().filter_map(ImportClause::as_item).collect()),
                    range: node.text_range(),
                    scope,
                });
            }
        } else {
            for clause in before {
                if clause.components.is_empty() {
                    continue;
                }
                self.model.module_loads.push(ModuleLoad {
                    kind,
                    path: clause.path(),
                    alias: clause.alias.map(|(name, _)| name),
                    items: None,
                    range: clause.range,
                    scope,
                });
            }
        }
        for child in node.descendants().skip(1) {
            if child.kind() == SyntaxKind::INTERPOLATION {
                self.walk_node(&child, scope);
            }
        }
        // `import A.$B` leaves the interpolation as statement-level tokens.
        let mut after_dollar = false;
        for element in node.children_with_tokens() {
            if let Some(token) = element.into_token() {
                match token.kind() {
                    SyntaxKind::DOLLAR => after_dollar = true,
                    SyntaxKind::IDENT if after_dollar => {
                        self.record_token_read(&token, scope);
                        after_dollar = false;
                    }
                    SyntaxKind::WHITESPACE => {}
                    _ => after_dollar = false,
                }
            }
        }
    }

    /// Record an `export`/`public` name list: identifiers, operators, macro
    /// names (`@` retained), and parenthesized names become entries resolved
    /// against the statement's global scope (resolution marks the binding
    /// used — exported is used); interpolations are ordinary reads and
    /// unresolved names deliberately stay out of the free reads.
    fn handle_name_list(&mut self, node: &SyntaxNode, scope: ScopeId) {
        let visibility = if node.kind() == SyntaxKind::EXPORT_STMT {
            Visibility::Exported
        } else {
            Visibility::Public
        };
        // `public` is a contextual keyword: the statement's leading token is
        // a plain IDENT to skip.
        let mut skip_keyword = node.kind() == SyntaxKind::PUBLIC_STMT;
        for element in node.children_with_tokens() {
            match element {
                rowan::NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::IDENT if skip_keyword => skip_keyword = false,
                    SyntaxKind::IDENT => {
                        self.push_export(t.text(), t.text_range(), scope, visibility);
                    }
                    k if k.is_operator() => {
                        self.push_export(t.text(), t.text_range(), scope, visibility);
                    }
                    _ => {}
                },
                rowan::NodeOrToken::Node(n) => match n.kind() {
                    SyntaxKind::MACRO_NAME => {
                        if let Some(t) = macro_name_ident(&n) {
                            let name = format!("@{}", t.text());
                            self.push_export(&name, n.text_range(), scope, visibility);
                        }
                    }
                    // `export (x)` unwraps to the name; `export ($a)` and
                    // bare `$a` interpolations read the enclosing value.
                    SyntaxKind::PAREN_EXPR => {
                        let mut inner = n.children();
                        match (inner.next(), inner.next()) {
                            (Some(name), None) if name.kind() == SyntaxKind::NAME => {
                                if let Some(t) = name_ident(&name) {
                                    self.push_export(t.text(), t.text_range(), scope, visibility);
                                }
                            }
                            _ => self.walk_children(&n, scope),
                        }
                    }
                    SyntaxKind::INTERPOLATION => self.walk_node(&n, scope),
                    _ => {}
                },
            }
        }
    }

    fn push_export(
        &mut self,
        name: &str,
        range: TextRange,
        scope: ScopeId,
        visibility: Visibility,
    ) {
        let binding = if let Some(bare) = name.strip_prefix('@') {
            self.resolve_macro_read(bare, scope)
        } else {
            self.find_in_scope(self.innermost_global(scope), name)
        };
        if let Some(b) = binding {
            self.model.bindings[b.0 as usize].read = true;
        }
        self.model.exports.push(ExportEntry {
            name: SmolStr::new(name),
            visibility,
            range,
            scope,
            binding,
        });
    }

    // --- declarations, type definitions, and modules ------------------------

    /// Walk a `local`/`global`/`const` statement: bare names are their own
    /// definition sites (nothing to record), assignment payloads run
    /// normally — the declare phase already routed the bindings.
    fn handle_declaration(&mut self, node: &SyntaxNode, scope: ScopeId) {
        for child in node.children() {
            match child.kind() {
                SyntaxKind::ASSIGNMENT_EXPR => self.handle_assignment(&child, scope),
                _ => self.walk_target(&child, scope, Access::Write),
            }
        }
    }

    /// `struct`/`abstract type`/`primitive type`: the name binds in the
    /// enclosing scope; curly type parameters, the supertype expression,
    /// fields, and inner constructors live in a `Struct` scope.
    fn handle_type_def(&mut self, node: &SyntaxNode, scope: ScopeId) {
        let struct_scope = self.push_scope(ScopeKind::Struct, Some(scope), node.text_range());
        if let Some(start) = node
            .children()
            .find(|c| c.kind() == SyntaxKind::SIGNATURE)
            .and_then(|sig| sig.children().next())
        {
            self.walk_type_signature(&start, scope, struct_scope);
        }
        if let Some(body) = node.children().find(|c| c.kind() == SyntaxKind::BLOCK) {
            // Fields bind; anything else (inner constructors, docstrings)
            // declares and walks as usual inside the struct scope.
            for stmt in body.children() {
                match stmt.kind() {
                    SyntaxKind::NAME | SyntaxKind::TYPE_ANNOTATION => {}
                    _ => self.declare_node(&stmt, struct_scope),
                }
            }
            for stmt in body.children() {
                match stmt.kind() {
                    SyntaxKind::NAME => self.bind_field(&stmt, struct_scope),
                    SyntaxKind::TYPE_ANNOTATION => {
                        let (pattern, types) = annotation_parts(&stmt);
                        if let Some(pattern) = pattern {
                            self.bind_field(&pattern, struct_scope);
                        }
                        for ty in types {
                            self.walk_node(&ty, struct_scope);
                        }
                    }
                    _ => self.walk_node(&stmt, struct_scope),
                }
            }
        }
    }

    fn bind_field(&mut self, node: &SyntaxNode, scope: ScopeId) {
        if node.kind() == SyntaxKind::NAME
            && let Some(token) = name_ident(node)
            && token.text() != "_"
        {
            self.push_binding(token.text(), BindingKind::Field, scope, token.text_range());
        }
    }

    /// The signature of a type definition: `Foo`, `Foo{T<:Real}`, possibly
    /// `<: Super`. The name is a write in the enclosing scope; type
    /// parameters bind in the struct scope, where the supertype expression
    /// is then walked (it may reference them).
    fn walk_type_signature(
        &mut self,
        start: &SyntaxNode,
        enclosing: ScopeId,
        struct_scope: ScopeId,
    ) {
        match start.kind() {
            SyntaxKind::NAME => {
                if let Some(token) = name_ident(start) {
                    self.write_name(&token, enclosing, Access::Write);
                }
            }
            SyntaxKind::CURLY_EXPR => {
                let mut children = start.children();
                if let Some(name) = children.next()
                    && name.kind() == SyntaxKind::NAME
                    && let Some(token) = name_ident(&name)
                {
                    self.write_name(&token, enclosing, Access::Write);
                }
                for args in start
                    .children()
                    .filter(|c| c.kind() == SyntaxKind::ARG_LIST)
                {
                    for arg in args.children() {
                        self.bind_type_param_spec(&arg, struct_scope);
                    }
                }
            }
            SyntaxKind::BINARY_EXPR | SyntaxKind::COMPARISON_EXPR => {
                let mut children = start.children();
                if let Some(name_part) = children.next() {
                    self.walk_type_signature(&name_part, enclosing, struct_scope);
                }
                for supertype in children {
                    self.walk_node(&supertype, struct_scope);
                }
            }
            _ => self.walk_node(start, struct_scope),
        }
    }

    /// `module M ... end`: the name binds in the enclosing scope; the body
    /// is a fresh global scope that does *not* see enclosing names.
    fn handle_module(&mut self, node: &SyntaxNode, scope: ScopeId) {
        if let Some(name) = node
            .children()
            .find(|c| c.kind() == SyntaxKind::SIGNATURE)
            .and_then(|sig| sig.children().find(|c| c.kind() == SyntaxKind::NAME))
            && let Some(token) = name_ident(&name)
        {
            self.write_name(&token, scope, Access::Write);
        }
        let body = node.children().find(|c| c.kind() == SyntaxKind::BLOCK);
        let range = body
            .as_ref()
            .map_or_else(|| node.text_range(), |b| b.text_range());
        let module_scope = self.push_scope(ScopeKind::Module, Some(scope), range);
        if let Some(body) = body {
            self.declare_in(&body, module_scope);
            self.walk_children(&body, module_scope);
        }
    }

    // --- block scopes -------------------------------------------------------

    /// `let a = 1, b, c = a ... end`: one chained scope per binding, so each
    /// right-hand side sees the previous bindings and `let x = x` reads the
    /// outer `x`. The body gets a scope of its own even without bindings.
    fn handle_let(&mut self, node: &SyntaxNode, scope: ScopeId) {
        let end = node.text_range().end();
        let mut current = scope;
        if let Some(bindings) = node
            .children()
            .find(|c| c.kind() == SyntaxKind::LET_BINDINGS)
        {
            for item in bindings.children() {
                let range = TextRange::new(item.text_range().start(), end);
                if item.kind() == SyntaxKind::ASSIGNMENT_EXPR {
                    let mut parts = item.children();
                    let target = parts.next();
                    for rhs in parts {
                        self.walk_node(&rhs, current);
                    }
                    current = self.push_scope(ScopeKind::Let, Some(current), range);
                    if let Some(target) = target {
                        self.bind_param_pattern(&target, current, BindingKind::LetVar);
                    }
                } else {
                    current = self.push_scope(ScopeKind::Let, Some(current), range);
                    self.bind_param_pattern(&item, current, BindingKind::LetVar);
                }
            }
        }
        if let Some(body) = node.children().find(|c| c.kind() == SyntaxKind::BLOCK) {
            let body_scope = self.push_scope(ScopeKind::Let, Some(current), body.text_range());
            self.declare_in(&body, body_scope);
            self.walk_children(&body, body_scope);
        }
    }

    /// One `FOR_BINDING` clause list: each clause's iterable is walked in
    /// the scope outside its variable (so `for x in x` reads the outer `x`,
    /// and later iterables see earlier variables), then the variable binds
    /// in a fresh chained scope. Returns the innermost scope.
    fn handle_for_binding(
        &mut self,
        node: &SyntaxNode,
        outer: ScopeId,
        end: rowan::TextSize,
        kind: ScopeKind,
    ) -> ScopeId {
        let mut current = outer;
        let clauses: Vec<SyntaxNode> = node.children().collect();
        let mut i = 0;
        while i < clauses.len() {
            let target = &clauses[i];
            if let Some(iterable) = clauses.get(i + 1) {
                self.walk_node(iterable, current);
            }
            let range = TextRange::new(target.text_range().start(), end);
            current = self.push_scope(kind, Some(current), range);
            self.bind_param_pattern(target, current, BindingKind::ForVar);
            i += 2;
        }
        current
    }

    fn handle_for(&mut self, node: &SyntaxNode, scope: ScopeId) {
        let end = node.text_range().end();
        let mut current = scope;
        for child in node.children() {
            if child.kind() == SyntaxKind::FOR_BINDING {
                current = self.handle_for_binding(&child, current, end, ScopeKind::For);
            }
        }
        if current == scope {
            // Recovery: a `for` with no binding still scopes its body.
            current = self.push_scope(ScopeKind::For, Some(scope), node.text_range());
        }
        if let Some(body) = node.children().find(|c| c.kind() == SyntaxKind::BLOCK) {
            self.declare_in(&body, current);
            self.walk_children(&body, current);
        }
    }

    fn handle_while(&mut self, node: &SyntaxNode, scope: ScopeId) {
        let while_scope = self.push_scope(ScopeKind::While, Some(scope), node.text_range());
        self.declare_in(node, while_scope);
        self.walk_children(node, while_scope);
    }

    /// `try`/`catch`/`else`/`finally`: each clause is its own soft scope —
    /// variables from the `try` block are *not* visible in `catch`. The
    /// catch variable binds in the catch scope.
    fn handle_try(&mut self, node: &SyntaxNode, scope: ScopeId) {
        for child in node.children() {
            match child.kind() {
                SyntaxKind::BLOCK => {
                    let s = self.push_scope(ScopeKind::Try, Some(scope), child.text_range());
                    self.declare_in(&child, s);
                    self.walk_children(&child, s);
                }
                SyntaxKind::CATCH_CLAUSE => {
                    let s = self.push_scope(ScopeKind::Catch, Some(scope), child.text_range());
                    for part in child.children() {
                        match part.kind() {
                            SyntaxKind::NAME => {
                                self.bind_param_pattern(&part, s, BindingKind::CatchParam);
                            }
                            SyntaxKind::BLOCK => {
                                self.declare_in(&part, s);
                                self.walk_children(&part, s);
                            }
                            _ => self.walk_node(&part, s),
                        }
                    }
                }
                SyntaxKind::ELSE_CLAUSE | SyntaxKind::FINALLY_CLAUSE => {
                    let kind = if child.kind() == SyntaxKind::FINALLY_CLAUSE {
                        ScopeKind::Finally
                    } else {
                        ScopeKind::Try
                    };
                    let s = self.push_scope(kind, Some(scope), child.text_range());
                    self.declare_in(&child, s);
                    self.walk_children(&child, s);
                }
                _ => self.walk_node(&child, scope),
            }
        }
    }

    /// Comprehensions and generators: hard scopes for their iteration
    /// variables. A typed comprehension's element type is read in the
    /// enclosing scope; the element expression and `if` filters run in the
    /// innermost clause scope.
    fn handle_comprehension(&mut self, node: &SyntaxNode, scope: ScopeId) {
        let end = node.text_range().end();
        let mut children = node.children().peekable();
        if node.kind() == SyntaxKind::TYPED_COMPREHENSION
            && let Some(ty) = children.next()
        {
            self.walk_node(&ty, scope);
        }
        let rest: Vec<SyntaxNode> = children.collect();
        let mut current = scope;
        for child in &rest {
            if child.kind() == SyntaxKind::FOR_BINDING {
                current = self.handle_for_binding(child, current, end, ScopeKind::Comprehension);
            }
        }
        if current == scope {
            current = self.push_scope(ScopeKind::Comprehension, Some(scope), node.text_range());
        }
        for child in &rest {
            if child.kind() != SyntaxKind::FOR_BINDING {
                self.declare_node(child, current);
                self.walk_node(child, current);
            }
        }
    }

    // --- function-like scopes ----------------------------------------------

    /// `function`/`macro` definitions, long and bare form. The name binds in
    /// the enclosing scope (the declare phase already did); `where` type
    /// parameters, parameters, return type, and body live in a fresh
    /// function scope.
    fn handle_function_def(&mut self, node: &SyntaxNode, scope: ScopeId) {
        let signature = node
            .children()
            .find(|c| c.kind() == SyntaxKind::SIGNATURE)
            .and_then(|sig| sig.children().next());
        let body = node.children().find(|c| c.kind() == SyntaxKind::BLOCK);
        let fn_scope = self.push_scope(ScopeKind::Function, Some(scope), node.text_range());
        if let Some(start) = signature {
            self.walk_signature(start, scope, fn_scope);
        }
        if let Some(body) = body {
            self.declare_in(&body, fn_scope);
            self.walk_children(&body, fn_scope);
        }
    }

    /// Short-form `f(x) = rhs` (possibly under `where` clauses or a return
    /// annotation): a function definition whose body is the right-hand side.
    fn handle_short_form(&mut self, node: &SyntaxNode, scope: ScopeId, lhs: SyntaxNode) {
        let fn_scope = self.push_scope(ScopeKind::Function, Some(scope), node.text_range());
        self.walk_signature(lhs, scope, fn_scope);
        for rhs in node.children().skip(1) {
            self.declare_node(&rhs, fn_scope);
            self.walk_node(&rhs, fn_scope);
        }
    }

    /// Bind everything a signature introduces: `where` type parameters and
    /// parameters into `fn_scope`; the function name (a write in
    /// `enclosing`); the return type as reads in `fn_scope`. Qualified
    /// callees (`Base.foo`) and callable-object signatures are handled here
    /// too.
    fn walk_signature(&mut self, start: SyntaxNode, enclosing: ScopeId, fn_scope: ScopeId) {
        let (core, wheres, return_ty) = peel_signature(start);
        for spec in &wheres {
            self.bind_type_param_spec(spec, fn_scope);
        }
        match core {
            Some(core) if core.kind() == SyntaxKind::CALL_EXPR => {
                let mut children = core.children();
                if let Some(callee) = children.next() {
                    match callee.kind() {
                        SyntaxKind::NAME => {
                            if let Some(token) = name_ident(&callee) {
                                self.write_name(&token, enclosing, Access::Write);
                            }
                        }
                        // Callable object: `function (o::T)(x)` — the object
                        // pattern is a parameter of the method.
                        SyntaxKind::PAREN_EXPR | SyntaxKind::TUPLE_EXPR => {
                            self.bind_params(&callee, fn_scope, false);
                        }
                        // `Base.foo(x)`: a method extension, reads only.
                        _ => self.walk_node(&callee, enclosing),
                    }
                }
                for args in children {
                    if args.kind() == SyntaxKind::ARG_LIST {
                        self.bind_params(&args, fn_scope, false);
                    } else {
                        self.walk_node(&args, fn_scope);
                    }
                }
            }
            // Anonymous `function (x, y) ... end`.
            Some(core) if core.kind() == SyntaxKind::TUPLE_EXPR => {
                self.bind_params(&core, fn_scope, false);
            }
            // Bare `function f end`.
            Some(core) if core.kind() == SyntaxKind::NAME => {
                if let Some(token) = name_ident(&core) {
                    self.write_name(&token, enclosing, Access::Write);
                }
            }
            Some(core) => self.walk_node(&core, fn_scope),
            None => {}
        }
        if let Some(ty) = return_ty {
            self.walk_node(&ty, fn_scope);
        }
    }

    /// `x -> body`, `(x, y) -> body`: an anonymous function.
    fn handle_arrow(&mut self, node: &SyntaxNode, scope: ScopeId) {
        let fn_scope = self.push_scope(ScopeKind::Function, Some(scope), node.text_range());
        let mut children = node.children();
        if let Some(params) = children.next() {
            if params.kind() == SyntaxKind::TUPLE_EXPR {
                self.bind_params(&params, fn_scope, false);
            } else {
                self.bind_param_pattern(&params, fn_scope, BindingKind::Param);
            }
        }
        for body in children {
            self.declare_node(&body, fn_scope);
            self.walk_node(&body, fn_scope);
        }
    }

    /// `f(args) do x, y ... end`: the call runs in the enclosing scope; the
    /// `do` parameters and body form a function scope.
    fn handle_do(&mut self, node: &SyntaxNode, scope: ScopeId) {
        let params = node.children().find(|c| c.kind() == SyntaxKind::DO_PARAMS);
        let body = node.children().find(|c| c.kind() == SyntaxKind::BLOCK);
        for child in node.children() {
            if !matches!(child.kind(), SyntaxKind::DO_PARAMS | SyntaxKind::BLOCK) {
                self.walk_node(&child, scope);
            }
        }
        let start = params
            .as_ref()
            .or(body.as_ref())
            .map_or_else(|| node.text_range().start(), |n| n.text_range().start());
        let range = TextRange::new(start, node.text_range().end());
        let fn_scope = self.push_scope(ScopeKind::Function, Some(scope), range);
        if let Some(params) = params {
            self.bind_params(&params, fn_scope, false);
        }
        if let Some(body) = body {
            self.declare_in(&body, fn_scope);
            self.walk_children(&body, fn_scope);
        }
    }

    /// Bind a parameter list: an `ARG_LIST`/`TUPLE_EXPR` (positional until a
    /// `PARAMETERS` group switches to keyword parameters) or `DO_PARAMS`
    /// (bare patterns).
    fn bind_params(&mut self, list: &SyntaxNode, scope: ScopeId, keyword: bool) {
        let kind = if keyword {
            BindingKind::KeywordParam
        } else {
            BindingKind::Param
        };
        for child in list.children() {
            match child.kind() {
                SyntaxKind::ARG => {
                    for pattern in child.children() {
                        self.bind_param_pattern(&pattern, scope, kind);
                    }
                }
                // A parameter with a default; the default is evaluated in
                // the function scope, where earlier parameters are visible.
                SyntaxKind::KEYWORD_ARG => {
                    let mut parts = child.children();
                    if let Some(pattern) = parts.next() {
                        self.bind_param_pattern(&pattern, scope, kind);
                    }
                    for default in parts {
                        self.walk_node(&default, scope);
                    }
                }
                SyntaxKind::PARAMETERS => self.bind_params(&child, scope, true),
                _ => self.bind_param_pattern(&child, scope, kind),
            }
        }
    }

    fn bind_param_pattern(&mut self, node: &SyntaxNode, scope: ScopeId, kind: BindingKind) {
        match node.kind() {
            SyntaxKind::NAME => {
                if let Some(token) = name_ident(node)
                    && token.text() != "_"
                {
                    self.push_binding(token.text(), kind, scope, token.text_range());
                }
            }
            SyntaxKind::TYPE_ANNOTATION => {
                let (pattern, types) = annotation_parts(node);
                if let Some(pattern) = pattern {
                    self.bind_param_pattern(&pattern, scope, kind);
                }
                for ty in types {
                    self.walk_node(&ty, scope);
                }
            }
            SyntaxKind::SPLAT_EXPR
            | SyntaxKind::TUPLE_EXPR
            | SyntaxKind::ARG
            | SyntaxKind::PAREN_EXPR => {
                for child in node.children() {
                    self.bind_param_pattern(&child, scope, kind);
                }
            }
            _ => self.walk_node(node, scope),
        }
    }

    /// Bind the type parameters of one `where` clause spec: a bare `NAME`,
    /// a bound like `T <: Number` (the bound is a read), or a braced group.
    fn bind_type_param_spec(&mut self, spec: &SyntaxNode, scope: ScopeId) {
        match spec.kind() {
            SyntaxKind::NAME => {
                if let Some(token) = name_ident(spec) {
                    self.push_binding(
                        token.text(),
                        BindingKind::TypeParam,
                        scope,
                        token.text_range(),
                    );
                }
            }
            SyntaxKind::BRACES | SyntaxKind::ARG => {
                for child in spec.children() {
                    self.bind_type_param_spec(&child, scope);
                }
            }
            SyntaxKind::BINARY_EXPR | SyntaxKind::COMPARISON_EXPR => {
                let mut children = spec.children();
                if let Some(param) = children.next() {
                    self.bind_type_param_spec(&param, scope);
                }
                for bound in children {
                    self.walk_node(&bound, scope);
                }
            }
            _ => self.walk_node(spec, scope),
        }
    }

    /// `$x` and `$(expr)` in strings and commands: the payload is a read in
    /// the enclosing scope. The bare form holds a raw `IDENT` token.
    fn walk_interpolation(&mut self, node: &SyntaxNode, scope: ScopeId) {
        for element in node.children_with_tokens() {
            match element {
                rowan::NodeOrToken::Token(token) if token.kind() == SyntaxKind::IDENT => {
                    self.record_token_read(&token, scope);
                }
                rowan::NodeOrToken::Node(child) => self.walk_node(&child, scope),
                _ => {}
            }
        }
    }

    /// `@name args...`: the final name component is a read in the macro
    /// namespace; arguments are ordinary expressions. A qualified name
    /// (`Base.@time`, `@Base.time`) instead reads its root qualifier as a
    /// value and records the whole chain as a qualified read — it must not
    /// resolve to a local macro of the same name.
    fn walk_macro_call(&mut self, node: &SyntaxNode, scope: ScopeId) {
        for child in node.children() {
            if child.kind() == SyntaxKind::MACRO_NAME {
                self.walk_macro_name(&child, scope);
            } else {
                self.walk_node(&child, scope);
            }
        }
    }

    fn walk_macro_name(&mut self, name: &SyntaxNode, scope: ScopeId) {
        // Qualifier components are NAME nodes (`Base.@time`) or the leading
        // IDENT tokens of the `@Base.time` form; the final IDENT is the
        // macro's own name in either.
        let mut parts: Vec<(SmolStr, Option<SyntaxToken>)> = Vec::new();
        for element in name.children_with_tokens() {
            match element {
                rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::IDENT => {
                    parts.push((SmolStr::new(t.text()), Some(t)));
                }
                rowan::NodeOrToken::Node(n) if n.kind() == SyntaxKind::NAME => {
                    if let Some(t) = name_ident(&n) {
                        parts.push((SmolStr::new(t.text()), Some(t)));
                    }
                }
                _ => {}
            }
        }
        let Some((macro_name, macro_token)) = parts.pop() else {
            return;
        };
        if parts.is_empty() {
            // Unqualified: a read in the macro namespace.
            let binding = self.resolve_macro_read(&macro_name, scope);
            if let Some(b) = binding {
                self.model.bindings[b.0 as usize].read = true;
            }
            if let Some(token) = macro_token {
                self.push_ident(
                    token.text(),
                    token.text_range(),
                    scope,
                    Access::Read,
                    true,
                    binding,
                );
            }
            return;
        }
        // Qualified: the root is an ordinary value read; the chain is a
        // qualified read in the macro namespace.
        if let Some(token) = &parts[0].1 {
            self.record_token_read(token, scope);
        }
        let mut path: Vec<SmolStr> = parts.into_iter().map(|(text, _)| text).collect();
        path.push(SmolStr::new(format!("@{macro_name}")));
        self.model.qualified_reads.push(QualifiedRead {
            path,
            range: name.text_range(),
            scope,
            is_macro: true,
        });
    }

    /// Quoted code is data, not evaluated here — except interpolations,
    /// which splice in values from the enclosing scope.
    fn walk_quoted(&mut self, node: &SyntaxNode, scope: ScopeId) {
        for child in node.children() {
            if child.kind() == SyntaxKind::INTERPOLATION {
                self.walk_node(&child, scope);
            } else {
                self.walk_quoted(&child, scope);
            }
        }
    }
}

/// LHS shapes that bind names (vs mutate-through-value targets, which the
/// declare phase must still scan for nested assignments).
fn is_binding_target(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::NAME
            | SyntaxKind::TUPLE_EXPR
            | SyntaxKind::BARE_TUPLE_EXPR
            | SyntaxKind::ARG
            | SyntaxKind::SPLAT_EXPR
            | SyntaxKind::PAREN_EXPR
            | SyntaxKind::TYPE_ANNOTATION
    )
}
