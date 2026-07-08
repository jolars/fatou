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

    // --- declare phase -----------------------------------------------------

    /// Introduce the bindings assigned anywhere in `scope`'s own extent,
    /// without descending into nested scopes.
    fn declare_in(&mut self, node: &SyntaxNode, scope: ScopeId) {
        for child in node.children() {
            self.declare_node(&child, scope);
        }
    }

    fn declare_node(&mut self, node: &SyntaxNode, scope: ScopeId) {
        if creates_scope(node.kind()) {
            return;
        }
        match node.kind() {
            SyntaxKind::ASSIGNMENT_EXPR => {
                let mut children = node.children();
                let lhs = children.next();
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

    fn declare_target(&mut self, node: &SyntaxNode, scope: ScopeId) {
        match node.kind() {
            SyntaxKind::NAME => {
                if let Some(token) = name_ident(node) {
                    self.declare_name(&token, scope);
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
    /// target and nothing changes.
    fn declare_name(&mut self, token: &SyntaxToken, scope: ScopeId) {
        if token.text() == "_" {
            return;
        }
        if let AssignSlot::NewIn(target, kind) = self.resolve_assign(token.text(), scope) {
            self.push_binding(token.text(), kind, target, token.text_range());
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
                    self.push_ident(
                        token.text(),
                        token.text_range(),
                        scope,
                        Access::Read,
                        true,
                        None,
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
