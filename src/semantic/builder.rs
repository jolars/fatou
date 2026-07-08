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

    /// Resolve a `@name` read: the macro namespace only sees `macro`
    /// definitions.
    fn resolve_macro_read(&self, name: &str, scope: ScopeId) -> Option<BindingId> {
        let mut cursor = Some(scope);
        while let Some(id) = cursor {
            let hit = self.scope(id).bindings.iter().rev().copied().find(|&b| {
                let binding = &self.model.bindings[b.0 as usize];
                binding.kind == BindingKind::Macro && binding.name == name
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
            SyntaxKind::BINARY_EXPR if is_field_access(node) => {
                if let Some(base) = node.children().next() {
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
    /// namespace; arguments are ordinary expressions. Qualifiers
    /// (`Base.@time`) stay untracked until the import model lands.
    fn walk_macro_call(&mut self, node: &SyntaxNode, scope: ScopeId) {
        for child in node.children() {
            if child.kind() == SyntaxKind::MACRO_NAME {
                let name_token = child
                    .children_with_tokens()
                    .filter_map(|e| e.into_token())
                    .filter(|t| t.kind() == SyntaxKind::IDENT)
                    .last();
                if let Some(token) = name_token {
                    let binding = self.resolve_macro_read(token.text(), scope);
                    if let Some(b) = binding {
                        self.model.bindings[b.0 as usize].read = true;
                    }
                    self.push_ident(
                        token.text(),
                        token.text_range(),
                        scope,
                        Access::Read,
                        true,
                        binding,
                    );
                }
            } else {
                self.walk_node(&child, scope);
            }
        }
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
