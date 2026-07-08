//! The per-file semantic model: scope tree, bindings (definition site plus
//! read and write sites), and free reads, from one walk of the CST.
//!
//! This is the enabler for everything semantic in the language server
//! (completion, hover, go-to-definition, references, rename) and for the
//! semantic lints. The design follows arity's semantic model: flat arenas
//! with index ids, [`SmolStr`] names, and structural equality so the salsa
//! query backdates when an edit leaves the model unchanged.
//!
//! Julia's scoping rules are honored as they apply to a *file*
//! (non-interactive): the top level and each `module` body are global
//! scopes; function-like bodies, `let`, comprehensions, and struct bodies
//! are hard local scopes; `for`/`while`/`try` are soft local scopes. An
//! assignment targets the innermost matching local up the scope chain
//! (closures can assign captured variables); otherwise it creates a local in
//! the scope of the assignment, or a global at global scope. Locals are
//! hoisted: any assignment in a scope makes the name local to the whole
//! scope, regardless of textual position. The REPL's soft-scope-at-top-level
//! behavior (reusing the global) deliberately diverges here, matching what
//! `julia file.jl` does.

pub mod binding;
pub mod builder;
pub mod scope;

use rowan::{TextRange, TextSize};
use smol_str::SmolStr;

use crate::syntax::SyntaxNode;

pub use binding::{Binding, BindingId, BindingKind};
pub use scope::{Scope, ScopeId, ScopeKind};

/// How an identifier occurrence uses its variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    /// Augmented assignment (`+=` and friends): reads, then writes.
    ReadWrite,
}

/// One identifier occurrence that is not a definition site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentRef {
    pub name: SmolStr,
    pub range: TextRange,
    /// The innermost scope the identifier occurs in.
    pub scope: ScopeId,
    pub access: Access,
    /// `@name` macro reads live in the macro namespace and never resolve to
    /// value bindings.
    pub is_macro: bool,
    /// The resolved binding; `None` is a free read (a name this file does
    /// not bind — a Base, imported, or undefined symbol).
    pub binding: Option<BindingId>,
}

/// One occurrence of a binding, as reported by [`SemanticModel::occurrences`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Occurrence {
    pub range: TextRange,
    pub access: Access,
    /// Whether this is the binding's definition site.
    pub is_def: bool,
}

/// The semantic model of one file. Build with [`SemanticModel::build`]; in
/// the language server, prefer the cached salsa query.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SemanticModel {
    scopes: Vec<Scope>,
    bindings: Vec<Binding>,
    idents: Vec<IdentRef>,
}

impl SemanticModel {
    /// Build the model from a parse tree root in one walk.
    pub fn build(root: &SyntaxNode) -> Self {
        builder::build(root)
    }

    pub fn scopes(&self) -> &[Scope] {
        &self.scopes
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// All identifier occurrences (excluding definition sites), in source
    /// order.
    pub fn idents(&self) -> &[IdentRef] {
        &self.idents
    }

    pub fn scope(&self, id: ScopeId) -> &Scope {
        &self.scopes[id.0 as usize]
    }

    pub fn binding(&self, id: BindingId) -> &Binding {
        &self.bindings[id.0 as usize]
    }

    /// The innermost scope containing `offset`. Falls back to the file scope
    /// (which always exists and spans the file).
    pub fn scope_at(&self, offset: TextSize) -> ScopeId {
        let mut best = ScopeId(0);
        let mut best_len = self.scopes[0].range.len();
        for (i, scope) in self.scopes.iter().enumerate() {
            if scope.range.contains_inclusive(offset) && scope.range.len() <= best_len {
                best = ScopeId(i as u32);
                best_len = scope.range.len();
            }
        }
        best
    }

    /// The bindings visible at `offset`, innermost first, shadowed names
    /// dropped. Resolution stops at the first global scope, like reads do.
    pub fn names_in_scope_at(&self, offset: TextSize) -> Vec<BindingId> {
        let mut seen: Vec<&SmolStr> = Vec::new();
        let mut out = Vec::new();
        let mut cursor = Some(self.scope_at(offset));
        while let Some(id) = cursor {
            let scope = self.scope(id);
            for &b in scope.bindings.iter().rev() {
                let name = &self.binding(b).name;
                if !seen.contains(&name) {
                    seen.push(name);
                    out.push(b);
                }
            }
            cursor = if scope.kind.is_global() {
                None
            } else {
                scope.parent
            };
        }
        out
    }

    /// The binding whose definition site contains `offset`, if any.
    pub fn binding_at(&self, offset: TextSize) -> Option<BindingId> {
        self.bindings
            .iter()
            .position(|b| b.def_range.contains_inclusive(offset))
            .map(|i| BindingId(i as u32))
    }

    /// The identifier occurrence containing `offset`, if any.
    pub fn ident_at(&self, offset: TextSize) -> Option<&IdentRef> {
        self.idents
            .iter()
            .find(|i| i.range.contains_inclusive(offset))
    }

    /// Every occurrence of `binding`: the definition site, then each
    /// resolved identifier, in source order of the identifiers.
    pub fn occurrences(&self, binding: BindingId) -> impl Iterator<Item = Occurrence> + '_ {
        let def = Occurrence {
            range: self.binding(binding).def_range,
            access: Access::Write,
            is_def: true,
        };
        std::iter::once(def).chain(
            self.idents
                .iter()
                .filter(move |i| i.binding == Some(binding))
                .map(|i| Occurrence {
                    range: i.range,
                    access: i.access,
                    is_def: false,
                }),
        )
    }

    /// The reads that no binding in this file satisfies, in source order.
    /// (Feeds the future `file_free_reads` firewall query.)
    pub fn free_reads(&self) -> impl Iterator<Item = &IdentRef> {
        self.idents
            .iter()
            .filter(|i| i.binding.is_none() && !i.is_macro)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model_of(src: &str) -> SemanticModel {
        SemanticModel::build(&crate::parser::parse(src).cst)
    }

    fn binding_names(m: &SemanticModel) -> Vec<&str> {
        m.bindings().iter().map(|b| b.name.as_str()).collect()
    }

    fn find(m: &SemanticModel, name: &str) -> BindingId {
        BindingId(
            m.bindings()
                .iter()
                .position(|b| b.name == name)
                .unwrap_or_else(|| panic!("no binding named {name}")) as u32,
        )
    }

    fn free_read_names(m: &SemanticModel) -> Vec<&str> {
        m.free_reads().map(|i| i.name.as_str()).collect()
    }

    #[test]
    fn top_level_assignment_creates_global_binding() {
        let m = model_of("x = 1");
        assert_eq!(binding_names(&m), ["x"]);
        let b = m.binding(find(&m, "x"));
        assert_eq!(b.kind, BindingKind::Global);
        assert_eq!(b.scope, ScopeId(0));
        assert!(!b.read);
    }

    #[test]
    fn reassignment_is_a_write_on_the_same_binding() {
        let m = model_of("x = 1\nx = 2");
        assert_eq!(binding_names(&m), ["x"]);
        let occ: Vec<_> = m.occurrences(find(&m, "x")).collect();
        assert_eq!(occ.len(), 2);
        assert!(occ[0].is_def);
        assert_eq!(occ[1].access, Access::Write);
        assert!(!m.binding(find(&m, "x")).read);
    }

    #[test]
    fn read_resolves_and_marks_binding() {
        let m = model_of("x = 1\ny = x");
        let x = find(&m, "x");
        assert!(m.binding(x).read);
        let read = m.idents().iter().find(|i| i.name == "x").unwrap();
        assert_eq!(read.binding, Some(x));
        assert_eq!(read.access, Access::Read);
    }

    #[test]
    fn forward_global_read_resolves_to_hoisted_binding() {
        let m = model_of("y = x\nx = 1");
        let x = find(&m, "x");
        assert!(m.binding(x).read);
        assert!(free_read_names(&m).is_empty());
    }

    #[test]
    fn unbound_name_is_a_free_read() {
        let m = model_of("y = sin(x)");
        assert_eq!(free_read_names(&m), ["sin", "x"]);
    }

    #[test]
    fn tuple_destructuring_binds_each_name() {
        let m = model_of("a, b = t");
        assert_eq!(binding_names(&m), ["a", "b"]);
        assert_eq!(free_read_names(&m), ["t"]);
    }

    #[test]
    fn paren_tuple_destructuring_binds_each_name() {
        let m = model_of("(a, b) = t");
        assert_eq!(binding_names(&m), ["a", "b"]);
    }

    #[test]
    fn chained_assignment_binds_both_targets() {
        let m = model_of("a = b = 1");
        assert_eq!(binding_names(&m), ["a", "b"]);
    }

    #[test]
    fn augmented_assignment_reads_and_writes() {
        let m = model_of("x = 1\nx += 2");
        let x = find(&m, "x");
        assert!(m.binding(x).read);
        let occ: Vec<_> = m.occurrences(x).filter(|o| !o.is_def).collect();
        assert_eq!(occ.len(), 1);
        assert_eq!(occ[0].access, Access::ReadWrite);
    }

    #[test]
    fn annotated_assignment_binds_name_and_reads_type() {
        let m = model_of("x::Int = 1");
        assert_eq!(binding_names(&m), ["x"]);
        assert_eq!(free_read_names(&m), ["Int"]);
    }

    #[test]
    fn index_assignment_reads_base_without_binding() {
        let m = model_of("x[i] = v");
        assert!(binding_names(&m).is_empty());
        assert_eq!(free_read_names(&m), ["x", "i", "v"]);
        let x = m.idents().iter().find(|i| i.name == "x").unwrap();
        assert_eq!(x.access, Access::Read);
    }

    #[test]
    fn field_assignment_reads_base_and_skips_field_name() {
        let m = model_of("x.f = v");
        assert!(binding_names(&m).is_empty());
        assert_eq!(free_read_names(&m), ["x", "v"]);
    }

    #[test]
    fn qualified_access_reads_only_the_root() {
        let m = model_of("a.b.c");
        assert_eq!(free_read_names(&m), ["a"]);
    }

    #[test]
    fn call_site_keyword_name_is_not_a_read() {
        let m = model_of("f(x = v)");
        assert_eq!(free_read_names(&m), ["f", "v"]);
        assert!(binding_names(&m).is_empty());
    }

    #[test]
    fn string_interpolation_reads() {
        let m = model_of("x = 1\ns = \"a $x b $(x + y)\"");
        let x = find(&m, "x");
        assert!(m.binding(x).read);
        assert_eq!(free_read_names(&m), ["y"]);
    }

    #[test]
    fn begin_and_if_blocks_do_not_scope() {
        let m = model_of("begin\n    x = 1\nend\nif c\n    y = 2\nend");
        for name in ["x", "y"] {
            let b = m.binding(find(&m, name));
            assert_eq!(b.scope, ScopeId(0), "{name} should be file-scope");
            assert_eq!(b.kind, BindingKind::Global);
        }
    }

    #[test]
    fn occurrences_do_not_double_count_the_def_site() {
        let m = model_of("x = 1\ny = x + x");
        let occ: Vec<_> = m.occurrences(find(&m, "x")).collect();
        assert_eq!(occ.len(), 3);
        assert_eq!(occ.iter().filter(|o| o.is_def).count(), 1);
    }

    // --- function scopes and parameters ------------------------------------

    fn kind_of(m: &SemanticModel, name: &str) -> BindingKind {
        m.binding(find(m, name)).kind
    }

    fn scope_kind_of(m: &SemanticModel, name: &str) -> ScopeKind {
        m.scope(m.binding(find(m, name)).scope).kind
    }

    #[test]
    fn long_form_function_binds_name_and_params() {
        let m = model_of("function f(x, y)\n    x + y\nend");
        assert_eq!(kind_of(&m, "f"), BindingKind::Function);
        assert_eq!(scope_kind_of(&m, "f"), ScopeKind::File);
        for p in ["x", "y"] {
            assert_eq!(kind_of(&m, p), BindingKind::Param);
            assert_eq!(scope_kind_of(&m, p), ScopeKind::Function);
            assert!(m.binding(find(&m, p)).read);
        }
        assert!(free_read_names(&m).is_empty());
    }

    #[test]
    fn short_form_function_binds_name_and_params() {
        let m = model_of("f(x) = x + 1");
        assert_eq!(kind_of(&m, "f"), BindingKind::Function);
        assert_eq!(kind_of(&m, "x"), BindingKind::Param);
        assert!(m.binding(find(&m, "x")).read);
        assert!(free_read_names(&m).is_empty());
    }

    #[test]
    fn function_body_locals_do_not_leak() {
        let m = model_of("function f()\n    tmp = 1\n    tmp\nend\ntmp");
        assert_eq!(kind_of(&m, "tmp"), BindingKind::Local);
        assert_eq!(scope_kind_of(&m, "tmp"), ScopeKind::Function);
        assert_eq!(free_read_names(&m), ["tmp"]);
    }

    #[test]
    fn keyword_params_after_semicolon() {
        let m = model_of("f(x, y = 2; k = 3, kw...) = x");
        assert_eq!(kind_of(&m, "x"), BindingKind::Param);
        assert_eq!(kind_of(&m, "y"), BindingKind::Param);
        assert_eq!(kind_of(&m, "k"), BindingKind::KeywordParam);
        assert_eq!(kind_of(&m, "kw"), BindingKind::KeywordParam);
    }

    #[test]
    fn default_reads_earlier_param() {
        let m = model_of("f(x, y = x + 1) = y");
        let x = find(&m, "x");
        assert!(m.binding(x).read);
        assert!(free_read_names(&m).is_empty());
    }

    #[test]
    fn destructured_and_annotated_params() {
        let m = model_of("f((a, b), x::Int) = a + b + x");
        for p in ["a", "b", "x"] {
            assert_eq!(kind_of(&m, p), BindingKind::Param);
        }
        assert_eq!(free_read_names(&m), ["Int"]);
    }

    #[test]
    fn unnamed_param_type_is_a_read() {
        let m = model_of("f(::Int) = 1");
        assert_eq!(binding_names(&m), ["f"]);
        assert_eq!(free_read_names(&m), ["Int"]);
    }

    #[test]
    fn where_clause_binds_type_params() {
        let m = model_of("f(x::T) where {T<:Number} = x");
        assert_eq!(kind_of(&m, "T"), BindingKind::TypeParam);
        assert!(m.binding(find(&m, "T")).read, "annotation reads T");
        assert_eq!(free_read_names(&m), ["Number"]);
    }

    #[test]
    fn chained_where_clauses_bind_all_params() {
        let m = model_of("f(x::S) where T where S = x");
        assert_eq!(kind_of(&m, "T"), BindingKind::TypeParam);
        assert_eq!(kind_of(&m, "S"), BindingKind::TypeParam);
        assert!(free_read_names(&m).is_empty());
    }

    #[test]
    fn return_type_is_read_in_function_scope() {
        let m = model_of("function f(x)::Int\n    x\nend");
        assert_eq!(kind_of(&m, "f"), BindingKind::Function);
        assert_eq!(free_read_names(&m), ["Int"]);
    }

    #[test]
    fn bare_function_form_binds_name() {
        let m = model_of("function f end");
        assert_eq!(binding_names(&m), ["f"]);
        assert_eq!(kind_of(&m, "f"), BindingKind::Function);
    }

    #[test]
    fn method_extension_binds_nothing() {
        let m = model_of("Base.foo(x) = x + 1");
        assert_eq!(binding_names(&m), ["x"]);
        assert_eq!(free_read_names(&m), ["Base"]);
    }

    #[test]
    fn callable_object_signature_binds_object_and_params() {
        let m = model_of("function (o::T)(x)\n    o.f + x\nend");
        assert_eq!(kind_of(&m, "o"), BindingKind::Param);
        assert_eq!(kind_of(&m, "x"), BindingKind::Param);
        assert_eq!(free_read_names(&m), ["T"]);
    }

    #[test]
    fn arrow_function_binds_params_and_captures() {
        let m = model_of("function outer(n)\n    xs -> xs .+ n\nend");
        assert_eq!(kind_of(&m, "xs"), BindingKind::Param);
        assert!(m.binding(find(&m, "n")).read, "closure captures n");
        assert!(free_read_names(&m).is_empty());
    }

    #[test]
    fn do_block_params_scope_to_the_body() {
        let m = model_of("xs = []\nmap(xs) do x\n    x + y\nend");
        assert_eq!(kind_of(&m, "x"), BindingKind::Param);
        assert!(m.binding(find(&m, "xs")).read, "call args read outer scope");
        assert_eq!(free_read_names(&m), ["map", "y"]);
    }

    #[test]
    fn closure_writes_captured_local() {
        let src = "function outer()\n    n = 0\n    bump() = (n += 1)\n    bump\nend";
        let m = model_of(src);
        assert_eq!(binding_names(&m), ["outer", "n", "bump"]);
        let n = find(&m, "n");
        assert_eq!(scope_kind_of(&m, "n"), ScopeKind::Function);
        assert!(m.binding(n).read);
        let writes: Vec<_> = m
            .idents()
            .iter()
            .filter(|i| i.binding == Some(n) && i.access == Access::ReadWrite)
            .collect();
        assert_eq!(writes.len(), 1);
    }

    #[test]
    fn forward_capture_assigns_the_hoisted_local() {
        let src = "function outer()\n    g() = (x = 2)\n    x = 1\nend";
        let m = model_of(src);
        let xs: Vec<_> = m.bindings().iter().filter(|b| b.name == "x").collect();
        assert_eq!(xs.len(), 1, "one hoisted local, not one per scope");
        assert_eq!(m.scope(xs[0].scope).kind, ScopeKind::Function);
    }

    #[test]
    fn mutual_recursion_resolves_both_ways() {
        let m = model_of("f() = g()\ng() = f()");
        assert!(m.binding(find(&m, "f")).read);
        assert!(m.binding(find(&m, "g")).read);
        assert!(free_read_names(&m).is_empty());
    }

    #[test]
    fn macro_def_binds_and_macro_call_resolves() {
        let m = model_of("macro m(ex)\n    ex\nend\n@m 1");
        assert_eq!(kind_of(&m, "m"), BindingKind::Macro);
        assert!(m.binding(find(&m, "m")).read);
        let call = m.idents().iter().find(|i| i.is_macro).unwrap();
        assert_eq!(call.binding, Some(find(&m, "m")));
    }

    #[test]
    fn macro_namespace_is_separate_from_values() {
        let m = model_of("time = 1\n@time f()");
        let call = m.idents().iter().find(|i| i.is_macro).unwrap();
        assert_eq!(call.binding, None, "@time must not resolve to the value");
        assert!(!m.binding(find(&m, "time")).read);
    }

    #[test]
    fn names_in_scope_sees_locals_params_and_globals() {
        let src = "g = 1\nfunction f(a)\n    b = 2\n    b\nend";
        let m = model_of(src);
        let offset = TextSize::from(src.rfind('b').unwrap() as u32);
        let visible: Vec<_> = m
            .names_in_scope_at(offset)
            .into_iter()
            .map(|b| m.binding(b).name.as_str())
            .collect();
        assert!(visible.contains(&"a"));
        assert!(visible.contains(&"b"));
        assert!(visible.contains(&"g"));
        assert!(visible.contains(&"f"));
    }
}
