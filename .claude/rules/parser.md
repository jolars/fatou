---
paths:
  - "crates/fatou-parser/**/*.rs"
  - "src/incremental.rs"
---

# Parser rules

Crate: `crates/fatou-parser` (`syntax`, `ast`, `parser`). Re-exported by the root
crate (`pub use fatou_parser::{ast, parser, syntax}`), so intra-repo consumers
keep writing `crate::parser::…`. Growing parity against JuliaSyntax has its own
rules file (`oracle.md`) and skill (`parser-parity`).

## Hard invariants

- **Losslessness** (Tenet 4). `reconstruct(text) == text`, byte for byte —
  whitespace, comments (including nested `#= =#`), line endings, all of it.
  Every new construct needs a losslessness assertion.
- **Errors never abort the parse.** Diagnostics ride a side channel
  (`diagnostics.rs`, `recovery.rs`); the tree is always produced. Prefer a
  stable, recoverable CST shape over early precision.
- **Semantics stay static.** No Julia runtime, no evaluation, ever. The parser
  recognizes lexical shape.
- **Parsing is the parser's job** (Tenet 3). If the formatter or linter trips
  over a mis-parse, fix it *here* — never paper over it downstream, and never
  let parsing logic creep into the formatter.
- **The crate must stay `wasm32-unknown-unknown`-clean**: no filesystem,
  process, thread, or clock use. The `wasm` job in `build-and-test.yml` is what
  enforces it. Salsa sits *above* the crate, in the root's `src/incremental.rs`.

## Pipeline

```
lex (lexer.rs) → Vec<Token>
parse_expr (expr.rs, Pratt) + structural.rs (recursive descent) → Vec<Event>
build_tree (tree_builder.rs) → rowan SyntaxNode (CST)
```

`core.rs` drives the loop; `events.rs` defines `Event` (start node, token,
finish node); `cursor.rs`, `context.rs`, `recovery.rs`, `diagnostics.rs` support
it. `syntax.rs` defines `SyntaxKind` (rowan-style `SCREAMING_SNAKE_CASE`) and
the `JuliaLanguage` binding. Unlike R, Julia has no `[[`/`]]` ambiguity, so
there is no bracket-rebalancer pass.

`unicode_ident.rs` and `unicode_ops.rs` are **generated** by
`scripts/generate-unicode-ident.jl` from the running Julia's own tables — never
hand-edit; regenerate on a Julia bump.

## Typed AST wrappers

`ast/` is a zero-cost typed **navigation** view over the CST (rust-analyzer's
mould), not a re-model. It is read-only: adding a wrapper changes no parser or
formatter output.

- `nodes.rs` — one `AstNode` newtype per node kind with typed accessors, plus
  the `Expr` sum. `Expr` carries an `Other(SyntaxNode)` catch-all so it stays
  **total** over expression kinds as the grammar grows: an operand of a
  not-yet-wrapped kind round-trips through `Other` rather than vanishing.
- `tokens.rs` — the `AstToken` trait (rowan ships only `AstNode`) and typed
  token newtypes. `Operator::can_cast` is `SyntaxKind::is_operator`, the one
  shared operator predicate; `child_token::<T>` is the typed-token analogue of
  `support::child`.
- `traits.rs` — the `Has*` shape traits (`HasArgList`, `HasBody`,
  `HasCondition`) so `arg_list()`/`body()`/`condition()` are one contract.

**To grow it:** add the `ast_node!`/`ast_token!` entry, add accessors via
`support::child`/`children`/`token`/`child_token`, impl any relevant `Has*`
trait, re-export from `ast.rs`, and add an accessor unit test.

**Who goes through it:** the linter, code actions, the semantic builder, the LSP
handlers, `project.rs`. **Who doesn't, deliberately:** the formatter (it works
raw CST — its transparent fallback is what guards losslessness), and the
polymorphic kind-classification walkers (`lsp/symbols.rs`, `lsp/folding.rs`,
`lsp/semantic_tokens.rs`), where single-kind wrappers would add code.

## Incrementality (Tenet 2)

- `reparse.rs` holds the reparse tiers; `edit.rs` the edit model. Any grammar
  change has to survive them.
- `src/incremental.rs` wraps parse as a salsa query. Splicing hints
  (`PrevParse` + staged `Edit`s) live *beside* salsa, not inside it, and are
  **hints only**: every route through `parsed_document` returns exactly what
  `parse(text)` would. A cold, stale, or evicted cache costs a full parse and
  nothing else.
- **Store green nodes in salsa, never red** — `SyntaxNode` is not `Send`/`Eq`.

## Testing

- Fixtures: `crates/fatou-parser/tests/fixtures/parser/<case>/input.jl`;
  `parser_snapshots.rs` snapshots the CST plus diagnostics and asserts
  losslessness. Also `incremental_reparse.rs` and `line_endings.rs`.
- `insta` snapshots live in `crates/fatou-parser/tests/snapshots/`. Review with
  `cargo insta review`; **never accept a snapshot you have not read**.
- Reparse performance: `cargo bench -p fatou-parser --features bench --bench
  reparse` (`task bench-reparse`), which needs `task bench-corpus` first.
