---
paths:
  - "src/semantic.rs"
  - "src/semantic/**/*.rs"
  - "src/project.rs"
  - "src/resolve.rs"
  - "src/incremental.rs"
  - "tests/cfg.rs"
  - "tests/resolve.rs"
  - "tests/salsa_incremental.rs"
---

# Semantic, resolution, and project rules

Four deliberately separate layers: `src/semantic/` is **strictly single-file**,
`src/project.rs` is the range-free projection firewall, `src/resolve.rs` is the
one name-resolution order, and `src/incremental.rs` wires them into salsa. Keep
cross-file logic out of `semantic/`.

## Semantic model (single file)

Scope tree, bindings (definition site plus read and write sites), free reads,
imports, signatures, and a control-flow graph — all from **one walk of the CST**
(`builder.rs`, `scope.rs`, `binding.rs`, `import.rs`, `signature.rs`, `cfg.rs`).

- **No Julia runtime, no evaluation, ever.**
- Flat arenas with index ids, `SmolStr` names, and **structural equality**, so
  the salsa query backdates when an edit leaves the model unchanged.
- Julia's scoping is honored as it applies to a *file* (non-interactive): the
  top level and each `module` body are global scopes; function-like bodies,
  `let`, comprehensions, and struct bodies are hard local scopes; `for`/`while`/
  `try` are soft local scopes. Locals are **hoisted** — any assignment in a scope
  makes the name local to the whole scope regardless of textual position. The
  REPL's soft-scope-at-top-level behavior deliberately diverges; we match what
  `julia file.jl` does.
- `collect_import_clauses` is shared with the index harvester, so a module's
  load surface is read exactly one way.

## Resolution order (`resolve.rs`)

**One masking order, shared by every consumer** — completion, hover,
go-to-definition, and the `undefined-name` lint must all agree. Julia layers
four tiers, innermost wins:

1. local scopes (up the scope chain, including file/module globals);
2. explicit imports (`import X`, `import X: a`, `using X: a`) — these are file
   bindings too, so tiers 1–2 together are just "does the name bind in this
   file?", already answered by the `SemanticModel`;
3. `using`'d exports of a whole-module `using X`, in source order;
4. Base/Core implicit.

`Resolver::resolve` returns the first hit; `Resolver::visible` enumerates every
visible name in the same order with shadowed names dropped, for completion.
**Add a new consumer here, not beside here.** Macros resolve in a parallel
`Namespace::Macro`: `@time` never resolves to a value `time`.

## Project projections (`project.rs`)

- **Per-file projections are deliberately range-free.** Stripping text ranges is
  what lets a projection backdate across a body edit so the project-level memos
  survive. `tests/salsa_incremental.rs` guards this — **adding a range to a
  projection will look harmless and will silently cost every keystroke a graph
  rebuild.**
- Order-independent containers (`BTreeSet`) for the same reason.
- The name-set projections read the `SemanticModel`; `include_edges` reads the
  parse tree directly, because an `include` is an ordinary call, not a binding.

## Incrementality

- `src/incremental.rs` models file text → CST → semantic model → resolution as
  salsa queries. The parser crate itself stays salsa-free.
- **Store green nodes in salsa, never red** (`SyntaxNode` is not `Send`/`Eq`).
- Whole-value leaves (`LibraryPackages` and friends) wrap their maps so the
  model types stay salsa-free and each value is an `Arc` — replacing one package
  clones only pointers.
- Salsa is **strictly single-writer**. Anything that writes must respect the
  LSP's analysis-thread ownership (`.claude/rules/lsp.md`).
