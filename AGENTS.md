# Agent Instructions

This file provides guidance to coding agents when working with code in this
repository.

## Project

Fatou is a Rust CLI providing a language server, formatter, and linter for the
Julia language. Single-crate Cargo package (binary and library crate both named
`fatou`, edition 2024), not a workspace.

The design follows rust-analyzer (and the author's R tool, `arity`, on which
this is modeled directly):

- lossless `rowan` CST trees,
- `salsa` for the incremental database,
- `lsp-server` for the language-server transport,
- an event-pipeline parser built for incremental reparse.

**Strategy (see `TODO.md`):** bring the parser + formatter foundation toward
completion *first*; the linter and the richer language-server features are
deferred to later phases. When in doubt about scope/priority, `TODO.md` is the
live roadmap and records known issues and follow-ups.

The dev environment is provided via `devenv`/Nix (`devenv.nix`, `devenv.yaml`)
and includes a Julia toolchain.

## Tenets

1. **Deterministic, rule-based formatting.** Output is decided solely by the
   formatter's rules and the layout engine. Push back against hard-coding
   special cases for specific constructs. Fatou does **not** honor "persistent
   line breaks": the input's existing line breaks never influence the result.
2. **Incremental parsing is first-class**, not an afterthought. Parser/CST work
   must keep the `salsa`-based reparse path (`src/incremental.rs`) viable.
3. **Parsing is the parser's job.** Never paper over parser mistakes in the
   formatter, and never let parsing logic creep into the formatter. If the
   formatter hits something the parser handled wrong, fix it in the parser.
4. **Losslessness is the parser's job.** The parser preserves all text
   (whitespace, comments, etc.) so that `reconstruct(text) == text` always. The
   formatter can assume the CST is lossless and focus on layout.
5. **Autofixes never introduce formatting errors.** A lint fix is not a
   formatter, but it must never make formatted code unformatted:
   `format` → `lint --fix` → `format --check` must pass. Make each fix
   format-clean by construction (or withhold it for that shape); don't run the
   formatter inside `--fix`.

## Runic compatibility (soft target)

Fatou tracks a **soft, one-directional compatibility target** with the
[Runic.jl](https://github.com/fredrikekre/Runic.jl) formatter — Julia's
deterministic, no-configuration formatter, and the philosophical match for
Tenet 1. This is **strictly subordinate to Tenet 1** and is **never a quality
gate**. We do not match Runic; we measure how often Runic would leave Fatou's
output unchanged, treating its maturity as a free differential oracle for our
own inconsistencies.

- The gauge (once it lands) measures the *fixed point*
  `runic(fatou(x)) == fatou(x)`, not a head-to-head diff. This cancels out the
  persistent-line-break difference by construction, leaving only genuine rule
  divergences.
- Divergences triage into two buckets. **Adopt** when Runic's output is simply
  more idiomatic and Fatou is being inconsistent (fix the rule). **Record** when
  the divergence is a deliberate Fatou choice (an allowlist entry with a
  rationale).
- Diverging from Runic is allowed but should **raise tension**: a conscious,
  documented decision, never a silent one.

The harness itself is deferred (`TODO.md`).

## Parser oracle (planned)

The intended differential oracle for the parser is **JuliaSyntax.jl** (the
official Julia parser, itself a lossless green-tree design). Cases will be
ported into the parser fixtures as hardening input once the grammar is
substantial enough to compare. Not yet wired up (`TODO.md`).

## Commands

```sh
cargo build                       # dev build
cargo test                        # all tests
cargo test <substring>            # tests matching a name
cargo test --test parser_snapshots   # one integration test file
cargo clippy --all-targets --all-features -- -D warnings   # warnings are errors
cargo fmt -- --check              # keep changes rustfmt-clean
```

CLI usage:

```sh
cargo run -- parse <file.jl>                 # print CST; stdin if no file
cat file.jl | cargo run -- parse --verify --quiet   # losslessness round-trip
cargo run -- format <file.jl>                # format to stdout (stdin if omitted)
cargo run -- format --check <dir>            # check without writing; non-zero if any differ
cargo run -- lint --check <dir>              # lint
cargo run -- lsp                             # run the language server on stdio
```

Snapshot tests use `insta`: review/accept with `cargo insta review` /
`cargo insta accept`. Logging honors `RUST_LOG` (e.g. `RUST_LOG=debug`) via
`env_logger`. `task <name>` (Taskfile.yml) wraps the common workflows.

## Architecture

**Parse pipeline** (`src/parser/`, public API `parse`/`reconstruct` re-exported
from `src/parser.rs`): a lossless `rowan` CST built via an event-based pipeline.

```
lex (lexer.rs) → Vec<Token>
parse_expr (expr.rs, Pratt) + structural.rs (recursive descent) → Vec<Event>
build_tree (tree_builder.rs) → rowan SyntaxNode (CST)
```

- `core.rs` drives the loop; `events.rs` defines `Event` (start node / token /
  finish node); `cursor.rs`, `context.rs`, `diagnostics.rs`, `recovery.rs`
  support the parser. `src/syntax.rs` defines `SyntaxKind` (rowan-style
  `SCREAMING_SNAKE_CASE`) and the `JuliaLanguage` binding.
- **Losslessness is the core invariant:** all whitespace, newlines, and comments
  (including nested `#= =#`) are preserved; `reconstruct(text) == text`. The
  grammar is a deliberately small **walking skeleton** (literals, operators with
  Julia precedence, calls, indexing, and the `function`/`if`/`begin` block
  forms) and grows incrementally (`TODO.md`). Unlike R, Julia has no `[[`/`]]`
  bracket ambiguity, so there is no bracket-rebalancer pass.
- `src/ast/nodes.rs` (`src/ast.rs`) provides zero-cost typed AST wrappers over
  the CST via rowan's `AstNode` support.
- `src/incremental.rs` models file text → CST as a `salsa` query
  (`parsed_document`). The token/block reparse *splicing* is deferred — today a
  text edit triggers a full parse (still correct).

**Formatter** (`src/formatter/`, public API in `src/formatter.rs`): consumes the
CST and uses a Wadler/Prettier-style document IR (`ir.rs`) printed by a single
best-fit layout engine (`printer.rs`) that makes all line-break decisions.
`style.rs` is `FormatStyle`; `check.rs` exposes `check_paths`. Target style is
Runic.jl's. The per-construct rules are deferred; today `core::format` is a
lossless passthrough routed through the layout engine.

**Linter** (`src/linter/`): `check_paths` parses each file and reports
`LintStatus` (`Clean` / `Findings` / `ParseDiagnostics`); parse diagnostics
block linting a file. The `Rule` trait + registry (`rules.rs`), `# fatou-ignore`
suppression (`suppression.rs`), diagnostics (`diagnostic.rs`), and rendering
(`render.rs`) are in place; no rules ship yet.

**Language server** (`src/lsp.rs`, CLI `fatou lsp`): a stdio JSON-RPC server on
the `lsp-server` crate (rust-analyzer's transport). Single-threaded for now:
advertises full-document sync + document formatting, pushes parse diagnostics on
open/change, and formats on request. The dedicated-lint-thread + rayon read-pool
model (forced by salsa's single-writer constraint) is a deliberate later step
(`TODO.md`).

**File discovery** (`src/file_discovery.rs`): `collect_julia_files` walks paths
for `.jl` files (via `ignore`); rejects non-`.jl` explicit file paths.

**Config** (`src/config.rs`): `fatou.toml` with `[format]` (line_width,
indent_width) and `[lint]` (select, ignore). Defaults follow Julia conventions
(width 92, indent 4).

## Invariants & conventions

- Treat CI as the source of truth for quality gates (`.github/workflows/`):
  cross-platform build/test, `cargo-audit` + `cargo-deny`, clippy `-D warnings`,
  rustfmt check.
- Formatter output must be **idempotent** (`format(format(x)) == format(x)`).
  The parser and formatter test suites guard losslessness + idempotence.
- Use **test-driven development**: write the test first, watch it fail, then make
  it pass. For a bug, add a failing fixture/snapshot that reproduces it before
  the fix.

## Commits & versioning

- **Conventional Commits** (`type(scope): subject`) and **semantic versioning**.
- Subject line ≤ 60 chars (≤ 72 fine). Bodies short and to the point.
- **Never edit the changelog by hand** — `versionary` generates it.

## Testing layout

- Integration tests in `tests/*.rs`; fixtures in
  `tests/fixtures/{parser,formatter}/<case>/`. Parser fixtures hold `input.jl`
  (snapshot the CST + diagnostics, assert losslessness); formatter fixtures hold
  `input.jl` + `expected.jl`.
- `insta` snapshots live in `tests/snapshots/`.
- `tests/lsp.rs` drives the language server over an in-memory connection.
