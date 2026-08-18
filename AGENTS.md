# Agent Instructions

This file is the repository-wide contract for coding agents.

Put cross-cutting invariants here. Put subsystem details in
`.claude/rules/*.md` with `paths` frontmatter so they load only when relevant.

Rules files should stay terse and under 200 lines. Keep them as operational
policy, not history. If context belongs to chronology, store it in tests,
issues, or `git log`, not in memory instructions.

`TODO.md` is the live roadmap and priority source of truth.

## What this project is

Fatou is a Rust CLI for Julia with five main surfaces:

- parser (lossless CST, incremental reparse support)
- formatter (canonical layout engine)
- linter (semantic linting and fixes)
- LSP (editor-facing analysis and code actions)
- package index/environment (static project and dependency discovery)

It is a Cargo workspace (edition 2024) with:

- root package `fatou` (binary and library)
- `crates/fatou-parser`
- `crates/fatou-formatter`

The root crate re-exports parser modules so internal paths remain stable
(`fatou::parser`, `fatou::formatter`, etc.).

Both member crates must stay `wasm32-unknown-unknown`-clean: no filesystem,
process, thread, or clock usage.

## Tenets

1. Deterministic canonical formatting.
   Semantically equivalent inputs format identically.
2. Incremental parsing is first-class.
   Parser/CST changes must preserve incremental viability.
3. Parsing belongs in the parser.
   Do not patch parser mistakes in formatter or linter.
4. Losslessness belongs in the parser.
   `reconstruct(text) == text` byte-for-byte.

Additional global constraints:

- Formatter is the sole authority on layout.
- `lint --fix` is byte-range rewrite only; it never runs the formatter.
- Pipeline is fix-then-format.
- No Julia runtime evaluation anywhere in analysis or linting.

## Commands

```sh
cargo build --workspace
cargo test --workspace
cargo test --workspace <substring>
cargo test -p fatou-parser --test parser_snapshots
cargo test --test linter_rules
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

Useful command examples:

```sh
cat file.jl | cargo run -- parse --verify --quiet
cargo run -- parse --to sexpr <file.jl>
cargo run -- format --check <path>
cargo run -- lint --fix <path>
cargo run -- debug format <path>
```

`task <name>` wraps common flows; see `Taskfile.yml` and `task --list`.

## Architecture map

Paths are relative to each crate root.

- Parse pipeline (`crates/fatou-parser/src/parser/`): event-driven parser,
  lossless CST, incremental reparse strategy, and oracle s-expression projector.
- Typed AST (`crates/fatou-parser/src/ast/`): zero-cost read-only wrappers.
- Formatter engine (`crates/fatou-formatter/src/formatter/`): document IR plus
  single best-fit layout printer.
- Root formatter bridge (`src/formatter.rs`, `src/formatter/`): CLI integration
  and batch checking.
- Semantic model (`src/semantic/`): single-file scope/binding/import/signature/
  CFG analysis.
- Resolution (`src/resolve.rs`): single masking order shared by all consumers.
- Project projections (`src/project.rs`): range-free projections to preserve
  cross-file memo stability.
- Package index and environment (`src/index/`, `src/environment.rs`): static
  package API harvesting and Julia-like environment discovery.
- Linter (`src/linter/`): semantic rules, suppressions, autofixes, and docs
  generation.
- LSP (`src/lsp.rs`, `src/lsp/`): JSON-RPC server, analysis threading model,
  and feature handlers.
- Config (`src/config.rs`): `fatou.toml` schema and discovery.
- File discovery (`src/file_discovery.rs`): `.jl` walking and explicit path
  validation.

## Invariants and conventions

- CI quality gates are authoritative (`.github/workflows/`).
- Losslessness is mandatory.
- Formatter idempotence is mandatory.
- Do not hand-edit generated files.

Generated outputs include:

- `CHANGELOG.md` and all version fields (`versionary`)
- `docs/src/reference/cli.md`
- `docs/src/reference/rules.md`
- parser/LSP generated unicode and LaTeX symbol tables
- `src/index/fallback/*.txt`
- pinned oracle corpus artifacts

Performance claims require measurement and benchmark artifacts.

## Commits and versioning

- Use Conventional Commits (`type(scope): subject`) and semver.
- Keep subjects concise.
- Keep commits atomic by release area (root crate, member crate, `editors/`).

## Testing

Use TDD:

- write a failing test or fixture first
- implement the fix
- run relevant tests, then `cargo test --workspace`

Suite ownership:

- parser tests and fixtures: `crates/fatou-parser/tests/`
- formatter tests and fixtures: `crates/fatou-formatter/tests/`
- root integration suites (CLI/LSP/linter/semantic/index/config): `tests/*.rs`

Snapshot policy:

- review all snapshot changes with `cargo insta review`
- do not accept unread snapshots

Cross-platform policy:

- tests must handle Windows path semantics when asserting absolute paths or
  `file:` URIs

Smoke test policy:

- `.github/workflows/smoke-test.yml` runs `fatou debug format` over real Julia
  repositories and files categorized regressions

## Environment

Development uses Nix/devenv (`devenv.nix`, `devenv.yaml`).

Julia packages are Pkg-managed via the repo `Project.toml` and `Manifest.toml`.
The shell exports `JULIA_PROJECT=@.`. Julia is required for regenerating parser
oracle corpora and generated tables, not for normal build/test flows.

In remote container sessions, `.claude/hooks/session-start.sh` bootstraps tools
when available and continues when Julia cannot be provisioned.
