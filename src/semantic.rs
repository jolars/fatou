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
pub mod import;
pub mod scope;
pub mod signature;

use rowan::{TextRange, TextSize};
use smol_str::SmolStr;

use crate::syntax::SyntaxNode;

pub use binding::{Binding, BindingId, BindingKind};
pub use import::{
    ExportEntry, ImportItem, LoadKind, ModuleLoad, ModulePath, QualifiedRead, Visibility,
};
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
    module_loads: Vec<ModuleLoad>,
    exports: Vec<ExportEntry>,
    qualified_reads: Vec<QualifiedRead>,
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

    /// The file-internal nested-`module` path enclosing `scope`, outermost
    /// first: the names of the `module` scopes on the chain from `scope` up to
    /// (and including) `scope` itself when it is a module body. Empty at the file
    /// top level. Combined with the file's host module path, this places a
    /// position in the package's module tree (nested-`module` file membership).
    pub fn enclosing_module_path(&self, scope: ScopeId) -> Vec<SmolStr> {
        let mut path = Vec::new();
        let mut cursor = Some(scope);
        while let Some(id) = cursor {
            let scope = self.scope(id);
            if scope.kind == ScopeKind::Module
                && let Some(name) = &scope.module_name
            {
                path.push(name.clone());
            }
            cursor = scope.parent;
        }
        path.reverse();
        path
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

    /// The `using`/`import` clauses, in source order. (Feeds the package
    /// index's resolution order and the future firewall queries.)
    pub fn module_loads(&self) -> &[ModuleLoad] {
        &self.module_loads
    }

    /// The `export`/`public` names, in source order. (Feeds the future
    /// `file_exports` firewall query.)
    pub fn exports(&self) -> &[ExportEntry] {
        &self.exports
    }

    /// The qualified reads (`Foo.bar`, `Base.@time`), in source order,
    /// separate from the bare free reads. (Feeds the future
    /// `file_qualified_reads` firewall query.)
    pub fn qualified_reads(&self) -> &[QualifiedRead] {
        &self.qualified_reads
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
    fn curly_callee_reads_where_params() {
        // `P{T}() where T = new()`: the constructor's curly type arguments
        // read the `where` parameters, not the enclosing scope.
        let m = model_of("struct P{T}\n    P{T}() where T = new()\nend");
        let ts: Vec<_> = m.bindings().iter().filter(|b| b.name == "T").collect();
        assert_eq!(ts.len(), 2, "struct param and where param");
        assert!(ts[1].read, "the constructor callee reads the where param");
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

    // --- block scopes -------------------------------------------------------

    #[test]
    fn let_binds_and_body_reads() {
        let m = model_of("let a = 1\n    a + 1\nend");
        assert_eq!(kind_of(&m, "a"), BindingKind::LetVar);
        assert!(m.binding(find(&m, "a")).read);
    }

    #[test]
    fn let_shadow_reads_outer_on_rhs() {
        let m = model_of("x = 1\nlet x = x\n    x\nend");
        let bindings: Vec<_> = m.bindings().iter().filter(|b| b.name == "x").collect();
        assert_eq!(bindings.len(), 2);
        let outer = find(&m, "x");
        assert_eq!(m.binding(outer).kind, BindingKind::Global);
        let rhs = m.idents().iter().find(|i| i.name == "x").unwrap();
        assert_eq!(rhs.binding, Some(outer), "let rhs reads the outer x");
        assert!(m.bindings().iter().all(|b| b.name != "x" || b.read));
    }

    #[test]
    fn let_bindings_chain_left_to_right() {
        let m = model_of("let a = 1, b, c = a\n    c\nend");
        assert_eq!(binding_names(&m), ["a", "b", "c"]);
        let a = find(&m, "a");
        let read = m.idents().iter().find(|i| i.name == "a").unwrap();
        assert_eq!(read.binding, Some(a));
    }

    #[test]
    fn let_body_locals_do_not_leak() {
        let m = model_of("let\n    y = 2\nend\ny");
        assert_eq!(kind_of(&m, "y"), BindingKind::Local);
        assert_eq!(scope_kind_of(&m, "y"), ScopeKind::Let);
        assert_eq!(free_read_names(&m), ["y"]);
    }

    #[test]
    fn for_binds_loop_var_and_reads_iterable_outside() {
        let m = model_of("xs = []\nfor x in xs\n    x\nend");
        assert_eq!(kind_of(&m, "x"), BindingKind::ForVar);
        assert!(m.binding(find(&m, "xs")).read);
        assert!(m.binding(find(&m, "x")).read);
    }

    #[test]
    fn for_iterable_does_not_see_the_loop_var() {
        let m = model_of("for x in x\nend");
        let read = m.idents().iter().find(|i| i.name == "x").unwrap();
        assert_eq!(read.binding, None, "iterable x is the (free) outer x");
    }

    #[test]
    fn multi_clause_for_chains_scopes() {
        let m = model_of("xs = []\nfor i in xs, j in f(i)\n    j\nend");
        let i = find(&m, "i");
        let read = m
            .idents()
            .iter()
            .find(|r| r.name == "i" && r.access == Access::Read)
            .unwrap();
        assert_eq!(read.binding, Some(i), "second iterable sees the first var");
    }

    #[test]
    fn for_destructures_tuple_vars() {
        let m = model_of(
            "for (k, v) in pairs\n    k
end",
        );
        assert_eq!(kind_of(&m, "k"), BindingKind::ForVar);
        assert_eq!(kind_of(&m, "v"), BindingKind::ForVar);
    }

    #[test]
    fn soft_scope_assigns_enclosing_local() {
        let src =
            "function f()\n    s = 0\n    for i in 1:3\n        s = s + i\n    end\n    s\nend";
        let m = model_of(src);
        let s_bindings: Vec<_> = m.bindings().iter().filter(|b| b.name == "s").collect();
        assert_eq!(s_bindings.len(), 1, "loop body assigns the function local");
        assert_eq!(m.scope(s_bindings[0].scope).kind, ScopeKind::Function);
    }

    #[test]
    fn top_level_soft_scope_makes_a_new_local() {
        // Non-interactive file semantics: the loop-body assignment does NOT
        // reuse the global (Julia warns and creates a local).
        let m = model_of("x = 0\nfor i in 1:3\n    x = i\nend");
        let xs: Vec<_> = m.bindings().iter().filter(|b| b.name == "x").collect();
        assert_eq!(xs.len(), 2);
        assert_eq!(m.scope(xs[1].scope).kind, ScopeKind::For);
    }

    #[test]
    fn for_body_locals_do_not_leak() {
        let m = model_of("for i in 1:3\n    t = i\nend\nt");
        assert_eq!(free_read_names(&m), ["t"]);
    }

    #[test]
    fn while_scopes_condition_and_body() {
        let m = model_of("n = 3\nwhile n > 0\n    t = n\nend");
        assert!(m.binding(find(&m, "n")).read);
        assert_eq!(scope_kind_of(&m, "t"), ScopeKind::While);
    }

    #[test]
    fn catch_binds_its_variable_in_the_catch_scope() {
        let m = model_of("try\n    risky()\ncatch e\n    handle(e)\nend");
        assert_eq!(kind_of(&m, "e"), BindingKind::CatchParam);
        assert_eq!(scope_kind_of(&m, "e"), ScopeKind::Catch);
        assert!(m.binding(find(&m, "e")).read);
    }

    #[test]
    fn try_locals_are_not_visible_in_catch() {
        let m = model_of("try\n    t = 1\ncatch\n    t\nend");
        assert_eq!(scope_kind_of(&m, "t"), ScopeKind::Try);
        assert_eq!(free_read_names(&m), ["t"]);
    }

    #[test]
    fn finally_gets_its_own_scope() {
        let m = model_of("try\n    risky()\nfinally\n    t = 1\nend");
        assert_eq!(scope_kind_of(&m, "t"), ScopeKind::Finally);
    }

    #[test]
    fn comprehension_binds_var_and_reads_iterable_outside() {
        let m = model_of("xs = []\n[x^2 for x in xs if x > 1]");
        assert_eq!(kind_of(&m, "x"), BindingKind::ForVar);
        assert_eq!(scope_kind_of(&m, "x"), ScopeKind::Comprehension);
        assert!(m.binding(find(&m, "xs")).read);
        let x = find(&m, "x");
        let reads = m.idents().iter().filter(|i| i.binding == Some(x)).count();
        assert_eq!(reads, 2, "element and filter both read x");
    }

    #[test]
    fn generator_scopes_like_a_comprehension() {
        let m = model_of("sum(x for x in 1:10)");
        assert_eq!(kind_of(&m, "x"), BindingKind::ForVar);
        assert!(m.binding(find(&m, "x")).read);
    }

    #[test]
    fn typed_comprehension_type_is_read_outside() {
        let m = model_of("Int[x for x in xs]");
        assert_eq!(free_read_names(&m), ["Int", "xs"]);
    }

    #[test]
    fn comprehension_vars_do_not_leak() {
        let m = model_of("[x for x in 1:3]\nx");
        assert_eq!(free_read_names(&m), ["x"]);
    }

    // --- declarations, structs, and modules ---------------------------------

    #[test]
    fn local_declaration_shadows_enclosing_local() {
        let src = "function f()\n    x = 1\n    for i in 1:3\n        local x\n        x = 2\n    end\n    x\nend";
        let m = model_of(src);
        let xs: Vec<_> = m.bindings().iter().filter(|b| b.name == "x").collect();
        assert_eq!(xs.len(), 2);
        assert_eq!(m.scope(xs[1].scope).kind, ScopeKind::For);
        let loop_x = BindingId(m.bindings().iter().rposition(|b| b.name == "x").unwrap() as u32);
        let write = m
            .idents()
            .iter()
            .find(|i| i.name == "x" && i.access == Access::Write)
            .unwrap();
        assert_eq!(
            write.binding,
            Some(loop_x),
            "loop assigns the declared local"
        );
    }

    #[test]
    fn global_declaration_routes_assignment_to_the_global() {
        let m = model_of("x = 0\nfunction f()\n    global x\n    x = 1\nend");
        let xs: Vec<_> = m.bindings().iter().filter(|b| b.name == "x").collect();
        assert_eq!(xs.len(), 1);
        assert_eq!(m.scope(xs[0].scope).kind, ScopeKind::File);
        let write = m
            .idents()
            .iter()
            .find(|i| i.name == "x" && i.access == Access::Write)
            .unwrap();
        assert_eq!(write.binding, Some(find(&m, "x")));
    }

    #[test]
    fn global_assignment_creates_the_global_from_inside() {
        let m = model_of("function f()\n    global y = 2\nend");
        assert_eq!(kind_of(&m, "y"), BindingKind::Global);
        assert_eq!(scope_kind_of(&m, "y"), ScopeKind::File);
    }

    #[test]
    fn global_with_annotation_binds_and_reads_type() {
        let m = model_of("global a, b::Int");
        assert_eq!(kind_of(&m, "a"), BindingKind::Global);
        assert_eq!(kind_of(&m, "b"), BindingKind::Global);
        assert_eq!(free_read_names(&m), ["Int"]);
    }

    #[test]
    fn local_with_tuple_assignment() {
        let m = model_of("local m, n = g()");
        assert_eq!(kind_of(&m, "m"), BindingKind::Local);
        assert_eq!(kind_of(&m, "n"), BindingKind::Local);
        assert_eq!(free_read_names(&m), ["g"]);
    }

    #[test]
    fn const_binds_with_const_kind() {
        let m = model_of("const c = 1\nc");
        assert_eq!(kind_of(&m, "c"), BindingKind::Const);
        assert!(m.binding(find(&m, "c")).read);
    }

    #[test]
    fn struct_binds_name_fields_and_type_params() {
        let m = model_of("struct Foo{T<:Real} <: Bar{T}\n    x::T\n    y\nend");
        assert_eq!(kind_of(&m, "Foo"), BindingKind::Type);
        assert_eq!(scope_kind_of(&m, "Foo"), ScopeKind::File);
        assert_eq!(kind_of(&m, "T"), BindingKind::TypeParam);
        assert_eq!(kind_of(&m, "x"), BindingKind::Field);
        assert_eq!(kind_of(&m, "y"), BindingKind::Field);
        assert_eq!(scope_kind_of(&m, "x"), ScopeKind::Struct);
        assert!(m.binding(find(&m, "T")).read, "supertype and field read T");
        assert_eq!(free_read_names(&m), ["Real", "Bar"]);
    }

    #[test]
    fn struct_fields_do_not_leak() {
        let m = model_of("struct S\n    f\nend\nf");
        assert_eq!(free_read_names(&m), ["f"]);
    }

    #[test]
    fn mutable_struct_and_const_field() {
        let m = model_of("mutable struct Counter\n    n::Int\nend");
        assert_eq!(kind_of(&m, "Counter"), BindingKind::Type);
        assert_eq!(kind_of(&m, "n"), BindingKind::Field);
    }

    #[test]
    fn inner_constructor_is_a_function_in_the_struct_scope() {
        let m = model_of("struct P\n    x\n    P(a) = new(a)\nend");
        assert_eq!(kind_of(&m, "a"), BindingKind::Param);
        assert!(m.binding(find(&m, "a")).read);
        let ctor = m
            .bindings()
            .iter()
            .find(|b| b.name == "P" && b.kind == BindingKind::Function)
            .expect("constructor binding");
        assert_eq!(m.scope(ctor.scope).kind, ScopeKind::Struct);
        assert!(free_read_names(&m).contains(&"new"));
    }

    #[test]
    fn abstract_type_binds_name_and_reads_supertype() {
        let m = model_of("abstract type A <: B end");
        assert_eq!(kind_of(&m, "A"), BindingKind::Type);
        assert_eq!(free_read_names(&m), ["B"]);
    }

    #[test]
    fn module_binds_name_and_scopes_its_body() {
        let m = model_of("module M\ny = 1\nend");
        assert_eq!(kind_of(&m, "M"), BindingKind::Module);
        assert_eq!(scope_kind_of(&m, "M"), ScopeKind::File);
        assert_eq!(kind_of(&m, "y"), BindingKind::Global);
        assert_eq!(scope_kind_of(&m, "y"), ScopeKind::Module);
    }

    #[test]
    fn module_body_does_not_see_enclosing_globals() {
        let m = model_of("x = 1\nmodule M\ny = x\nend");
        assert!(!m.binding(find(&m, "x")).read);
        assert_eq!(free_read_names(&m), ["x"]);
    }

    #[test]
    fn module_locals_do_not_leak_out() {
        let m = model_of("module M\ny = 1\nend\ny");
        assert_eq!(free_read_names(&m), ["y"]);
    }

    // --- imports and exports -------------------------------------------------

    #[test]
    fn using_whole_module_binds_last_component() {
        let m = model_of("using A");
        assert_eq!(binding_names(&m), ["A"]);
        assert_eq!(kind_of(&m, "A"), BindingKind::Import);

        let m = model_of("using A.B");
        assert_eq!(binding_names(&m), ["B"]);
    }

    #[test]
    fn import_binds_last_component() {
        let m = model_of("import A.B.C");
        assert_eq!(binding_names(&m), ["C"]);
        let load = &m.module_loads()[0];
        assert_eq!(load.kind, LoadKind::Import);
        assert_eq!(load.path.leading_dots, 0);
        assert_eq!(load.path.components, ["A", "B", "C"]);
        assert_eq!(load.items, None);
    }

    #[test]
    fn import_alias_binds_alias() {
        let m = model_of("import A as Z");
        assert_eq!(binding_names(&m), ["Z"]);
        let load = &m.module_loads()[0];
        assert_eq!(load.path.components, ["A"]);
        assert_eq!(load.alias.as_deref(), Some("Z"));
    }

    #[test]
    fn using_items_bind_only_items() {
        let m = model_of("using A: x, y");
        assert_eq!(binding_names(&m), ["x", "y"]);
        let load = &m.module_loads()[0];
        assert_eq!(load.kind, LoadKind::Using);
        assert_eq!(load.path.components, ["A"]);
        let items = load.items.as_ref().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "x");
        assert_eq!(items[1].name, "y");
    }

    #[test]
    fn item_alias_binds_alias() {
        let m = model_of("import A: x as y");
        assert_eq!(binding_names(&m), ["y"]);
        let items = m.module_loads()[0].items.as_ref().unwrap();
        assert_eq!(items[0].name, "x");
        assert_eq!(items[0].alias.as_deref(), Some("y"));
    }

    #[test]
    fn import_comma_form_records_two_loads() {
        let m = model_of("import A, B");
        assert_eq!(binding_names(&m), ["A", "B"]);
        let loads = m.module_loads();
        assert_eq!(loads.len(), 2);
        assert_eq!(loads[0].path.components, ["A"]);
        assert_eq!(loads[1].path.components, ["B"]);
    }

    #[test]
    fn relative_import_counts_leading_dots() {
        let m = model_of("import ..A");
        assert_eq!(binding_names(&m), ["A"]);
        let load = &m.module_loads()[0];
        assert_eq!(load.path.leading_dots, 2);
        assert_eq!(load.path.components, ["A"]);
    }

    #[test]
    fn operator_import_binds_operator() {
        let m = model_of("import A: +");
        assert_eq!(binding_names(&m), ["+"]);
        assert_eq!(kind_of(&m, "+"), BindingKind::Import);
    }

    #[test]
    fn imported_name_resolves_reads() {
        let m = model_of("import A: f\nf()");
        assert!(m.binding(find(&m, "f")).read);
        assert!(free_read_names(&m).is_empty());
    }

    #[test]
    fn whole_module_using_leaves_exports_free() {
        let m = model_of("using A\nf()");
        assert_eq!(free_read_names(&m), ["f"]);
    }

    #[test]
    fn imported_macro_resolves_macro_calls() {
        let m = model_of("using X: @foo\n@foo 1");
        assert_eq!(binding_names(&m), ["@foo"]);
        assert_eq!(kind_of(&m, "@foo"), BindingKind::Import);
        assert!(m.binding(find(&m, "@foo")).read);
        let call = m.idents().iter().find(|i| i.is_macro).unwrap();
        assert_eq!(call.binding, Some(find(&m, "@foo")));
    }

    #[test]
    fn macro_import_does_not_satisfy_value_reads() {
        let m = model_of("using X: @foo\nfoo");
        assert_eq!(free_read_names(&m), ["foo"]);
    }

    #[test]
    fn interpolated_import_reads_not_binds() {
        let m = model_of("import $A");
        assert!(binding_names(&m).is_empty());
        assert!(m.module_loads().is_empty());
        assert_eq!(free_read_names(&m), ["A"]);
    }

    #[test]
    fn module_body_import_scopes_to_module() {
        let m = model_of("module M\nusing Inner: q\nend");
        assert_eq!(kind_of(&m, "q"), BindingKind::Import);
        assert_eq!(scope_kind_of(&m, "q"), ScopeKind::Module);
        let load = &m.module_loads()[0];
        assert_eq!(m.scope(load.scope).kind, ScopeKind::Module);
    }

    #[test]
    fn invalid_using_as_is_skipped() {
        let m = model_of("using A as B");
        assert!(binding_names(&m).is_empty());
        assert!(m.module_loads().is_empty());

        let m = model_of("using A as B: x");
        assert!(binding_names(&m).is_empty());
        assert!(m.module_loads().is_empty());
    }

    #[test]
    fn export_records_and_marks_read() {
        let m = model_of("f() = 1\nexport f");
        let entry = &m.exports()[0];
        assert_eq!(entry.name, "f");
        assert_eq!(entry.visibility, Visibility::Exported);
        assert_eq!(entry.binding, Some(find(&m, "f")));
        assert!(m.binding(find(&m, "f")).read);
    }

    #[test]
    fn export_before_definition_resolves() {
        let m = model_of("export f\nf() = 1");
        assert_eq!(m.exports()[0].binding, Some(find(&m, "f")));
    }

    #[test]
    fn export_undefined_is_not_a_free_read() {
        let m = model_of("export g");
        assert_eq!(m.exports()[0].binding, None);
        assert!(free_read_names(&m).is_empty());
    }

    #[test]
    fn export_operator_and_macro_names() {
        let m = model_of("macro m()\nend\nexport +, @m");
        let names: Vec<_> = m.exports().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["+", "@m"]);
        assert_eq!(m.exports()[1].binding, Some(find(&m, "m")));
        assert!(m.binding(find(&m, "m")).read);
    }

    #[test]
    fn export_paren_name_is_not_a_read() {
        let m = model_of("export (x)");
        assert_eq!(m.exports()[0].name, "x");
        assert!(free_read_names(&m).is_empty());
    }

    #[test]
    fn public_records_with_public_visibility() {
        let m = model_of("g() = 1\npublic g");
        let entry = &m.exports()[0];
        assert_eq!(entry.name, "g");
        assert_eq!(entry.visibility, Visibility::Public);
        assert_eq!(entry.binding, Some(find(&m, "g")));
    }

    #[test]
    fn public_assignment_is_not_a_name_list() {
        let m = model_of("public = 4");
        assert!(m.exports().is_empty());
        assert_eq!(binding_names(&m), ["public"]);
    }

    #[test]
    fn qualified_read_records_full_path() {
        let m = model_of("a.b.c");
        let q = &m.qualified_reads()[0];
        assert_eq!(q.path, ["a", "b", "c"]);
        assert!(!q.is_macro);
        assert_eq!(free_read_names(&m), ["a"], "only the root is an ident");
    }

    #[test]
    fn qualified_read_root_resolves_to_import() {
        let m = model_of("import A\nA.f()");
        assert!(m.binding(find(&m, "A")).read);
        assert_eq!(m.qualified_reads()[0].path, ["A", "f"]);
    }

    #[test]
    fn qualified_macro_records_and_reads_root() {
        let m = model_of("Base.@time f()");
        let q = &m.qualified_reads()[0];
        assert_eq!(q.path, ["Base", "@time"]);
        assert!(q.is_macro);
        assert_eq!(free_read_names(&m), ["Base", "f"]);
        assert!(
            !m.idents().iter().any(|i| i.is_macro),
            "a qualified macro is not a local macro-namespace read"
        );
    }

    #[test]
    fn deep_qualified_macro_records_and_reads_root() {
        // A multi-component qualifier parses as a nested field-access chain
        // under the `MACRO_NAME`; the whole chain is still one qualified read.
        let m = model_of("Base.Threads.@spawn f()");
        let q = &m.qualified_reads()[0];
        assert_eq!(q.path, ["Base", "Threads", "@spawn"]);
        assert!(q.is_macro);
        assert_eq!(free_read_names(&m), ["Base", "f"]);
        assert!(
            !m.idents().iter().any(|i| i.is_macro),
            "a qualified macro is not a local macro-namespace read"
        );
    }

    #[test]
    fn call_chains_are_not_qualified_reads() {
        let m = model_of("f(x).y");
        assert!(m.qualified_reads().is_empty());
        assert_eq!(free_read_names(&m), ["f", "x"]);
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
