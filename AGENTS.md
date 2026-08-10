# Agent Instructions

This file provides guidance to coding agents working in this repository. It
carries the things that are true everywhere: what fatou is, the tenets, the
commands, and the cross-cutting invariants.

**Per-subsystem directives live in `.claude/rules/*.md`**, path-scoped in
frontmatter so each loads only when you read that subsystem's files: `parser`,
`oracle` (JuliaSyntax parity + the Julia version pin), `formatter`, `linter`,
`lsp`, `semantic` (semantic, resolution, project, salsa), `index` (package
index + Julia environment), `config`, `docs` (docs site + benchmarks), and
`release` (packaging, workflows, versioning).

Keep each rule file terse and under 200 lines: a rule, the one clause that keeps
it from looking arbitrary, and a pointer. A rule that must hold *before* any
file is read belongs here instead — path-scoped rules only load once a matching
file is read, and they are not re-injected after a compaction.

Worked examples and issue archaeology belong in neither file: they live in the
issue tracker, in `git log`, and above all in named tests and fixtures, which
are what fails when a rule is violated. **`TODO.md` is the live roadmap** and
records known issues and follow-ups; when in doubt about scope or priority, it
is the source of truth.

## What this project is

Fatou is a Rust CLI providing a language server, formatter, and linter for the
Julia language. It is a Cargo workspace (edition 2024) whose root package,
`fatou` (binary *and* library), hosts the CLI, LSP, linter, semantic model,
resolution, project projections, and package index, and builds on two
independently published member crates:

- **`crates/fatou-parser`** — `syntax` (SyntaxKind, node pointers), `ast` (typed
  wrappers), `parser` (lossless CST parser + incremental reparse).
- **`crates/fatou-formatter`** — the formatting engine, for embedders such as a
  dprint plugin.

The root crate re-exports the parser crate's modules, and `src/formatter.rs`
bridges the engine while hosting the CLI-side batch `check` API — so
`fatou::parser`, `fatou::formatter`, etc. stay the paths everything uses.

**Both member crates must stay `wasm32-unknown-unknown`-clean** (no filesystem,
process, thread, or clock use); a dedicated CI job is what enforces it.

Beyond the crate the repo ships the distribution surfaces: a VS Code extension
(`editors/code`), npm packages (`npm/`), a PyPI package (via maturin), an AUR
package (`packaging/aur`), the docs site (`docs/`), and benchmarks (`bench/`).

The design follows rust-analyzer, and the author's R tool **`arity`**, on which
fatou is modeled directly: lossless `rowan` CST trees, `salsa` for the
incremental database, `lsp-server` for the language-server transport, and an
event-pipeline parser built for incremental reparse.

## Tenets

1. **Deterministic, canonical formatting.** Output is decided solely by the
   formatter's rules and the layout engine, never by how the input happens to be
   written. Semantically-equivalent inputs **must** format identically: the
   input's line breaks, whitespace, operator spelling (`in` vs `∈`, `a*b` vs
   `a * b`), and numeric-literal form never influence the result. Fatou does
   **not** honor "persistent line breaks"; it **fully reflows**, laying out each
   construct from scratch under `line_width` and breaking only where width or
   semantics require it. Push back against hard-coding special cases for
   specific constructs. Any deviation from full reflow is a deliberate, recorded
   choice, never silent non-determinism.
2. **Incremental parsing is first-class**, not an afterthought. Parser/CST work
   must keep the salsa-based reparse path (`src/incremental.rs`) viable.
3. **Parsing is the parser's job.** Never paper over parser mistakes in the
   formatter, and never let parsing logic creep into the formatter. If the
   formatter hits something the parser handled wrong, fix it in the parser.
4. **Losslessness is the parser's job.** The parser preserves all text
   (whitespace, comments, and the rest) so that `reconstruct(text) == text`
   always. The formatter may assume a lossless CST and focus on layout.

Because the formatter is the **sole authority on layout**, a lint fix is not a
formatter: `lint --fix` applies each fix as a byte-range replacement and never
runs the formatter. The pipeline is fix-then-format. Full policy in
`.claude/rules/linter.md`.

**Semantics stay static** everywhere: no Julia runtime, no evaluation, at any
point in the pipeline — including the package index, which reads Julia's on-disk
layout with fatou's own parser.

## Commands

```sh
cargo build --workspace           # dev build
cargo test --workspace            # all tests (bare `cargo test` runs only the root crate!)
cargo test --workspace <substring>                   # tests matching a name
cargo test -p fatou-parser --test parser_snapshots   # one member-crate test file
cargo test --test linter_rules                       # one root-crate test file (`ls tests/*.rs`)
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check        # keep changes rustfmt-clean
```

Subcommands are `parse`, `format`, `lint`, `lsp`, and `debug`
(`docs/src/reference/cli.md` is the generated reference). The non-obvious flags:

```sh
cat file.jl | cargo run -- parse --verify --quiet   # losslessness round-trip check
cargo run -- parse --to sexpr <file.jl>   # the JuliaSyntax-shaped oracle projection
cargo run -- format --check <path>        # check without writing; non-zero if any differ
cargo run -- lint --fix <path>            # safe autofixes only (--unsafe-fixes for the rest)
cargo run -- debug format <path>          # the smoke test's per-file invariant checker
```

All commands honor a `fatou.toml` found by an ancestor walk. `task <name>`
(`Taskfile.yml`, `task --list`) wraps the workflows: `lint`, `format`, `test`,
`audit`, `deny`, `docs-build`, `docs-preview`, `bench`, `bench-memory`,
`bench-corpus`, `bench-lsenv`, `bench-reparse`, and the logo/icon generators. `insta` snapshots are reviewed
with `cargo insta review`; logging honors `RUST_LOG` via `env_logger`.

## Architecture map

Paths are relative to the owning crate (`syntax`, `ast`, `parser` in
`crates/fatou-parser/src/`, the formatter in `crates/fatou-formatter/src/`,
everything else in the root crate's `src/`); each entry's directives are in the
matching `.claude/rules/` file.

- **Parse pipeline** (`parser/`) — lossless `rowan` CST via an event-based
  pipeline: `lex` → `parse_expr` (Pratt) + `structural` (recursive descent) →
  `build_tree`. `reparse.rs` holds the incremental strategies; `sexpr.rs` is the
  test-only projector behind the JuliaSyntax oracle. `ast/` is a zero-cost
  typed, read-only *navigation* view that consumers go through, while the
  formatter deliberately stays on raw CST.
- **Formatter** (`crates/fatou-formatter`) — a Wadler/Prettier-style document IR
  printed by a single best-fit layout engine that makes every line-break
  decision. Constructs without a rule lower *transparently*, so unhandled syntax
  stays byte-identical while rules land incrementally.
- **Semantic model** (`src/semantic/`) — strictly *single-file*: scope tree,
  bindings, free reads, imports, signatures, and a per-file CFG, from one walk.
- **Resolution** (`src/resolve.rs`) — the one name-masking order every consumer
  shares: locals → explicit imports → `using`'d exports → Base/Core implicit.
- **Project projections** (`src/project.rs`) — deliberately *range-free* per-file
  projections, the firewall that lets cross-file memos survive a body edit.
- **Package index** (`src/index/`, `src/environment.rs`) — harvests a Julia
  package's public API **without a Julia runtime**, and locates the active
  project/depot the way Julia's loader does. Falls back to a baked-in Base/Core
  export list when no install is found.
- **Linter** (`src/linter/`) — **purely semantic**; anything `format --check`
  catches belongs to the formatter. 38 rules across `correctness`, `suspicious`,
  and `readability`, with autofixes, `# fatou-ignore` suppression, and a
  generated rule reference.
- **Language server** (`src/lsp.rs` + `src/lsp/`) — stdio JSON-RPC on
  `lsp-server`, with a dedicated analysis thread owning the salsa database and
  a purpose-built read pool.
- **Config** (`src/config.rs`) — `fatou.toml`: `[format]`, `[lint]`, `[julia]`,
  and top-level excludes. Defaults follow Julia conventions (width 92, indent 4).
- **File discovery** (`src/file_discovery.rs`) — walks paths for `.jl` files via
  `ignore`; rejects non-`.jl` explicit file paths.

## Invariants and conventions

- **Treat CI as the source of truth for quality gates** (`.github/workflows/`):
  cross-platform build/test, the **wasm build** of both member crates,
  `cargo-audit` + `cargo-deny`, clippy `-D warnings`, and the rustfmt check.
- **Losslessness:** `reconstruct(text) == text`, byte for byte.
- **Idempotence:** `format(format(x)) == format(x)`, and the output reparses
  cleanly. Byte-identical output is the bar for a "behavior-preserving" refactor.
- **Never hand-edit generated files.** `CHANGELOG.md` and every version field
  (`versionary`); `docs/src/reference/{cli,rules}.md`; `unicode_ident.rs`,
  `unicode_ops.rs`, `lsp/latex_symbols.rs`, and `index/fallback/*.txt` (Julia
  script-generated); the pinned oracle corpus.
- Speed is **measured, not asserted**: benchmarks are opt-in, local, and never a
  gate (`.claude/rules/docs.md`).

## Commits and versioning

- **Conventional Commits** (`type(scope): subject`) and semantic versioning.
  Subject ≤ 60 chars where possible, ≤ 72 is fine. Bodies short and to the point.
- Keep commits **atomic per area** (root crate, member crate, `editors/`): the
  release tooling routes versions by path, and a commit spanning two areas
  produces muddled per-crate changelogs. Details in `.claude/rules/release.md`.

## Testing

**Use test-driven development.** Write the test first, watch it fail, then make
it pass. For a bug, always start with a failing fixture or snapshot that
reproduces it before touching the fix. **Run `cargo test --workspace`** — a bare
`cargo test` covers only the root crate.

- Integration tests live with their crate: parser suites and fixtures in
  `crates/fatou-parser/tests/` (`fixtures/parser/<case>/input.jl`; snapshot the
  CST + diagnostics, assert losslessness), the formatter gate in
  `crates/fatou-formatter/tests/` (`fixtures/formatter/<case>/` holds `input.jl`
  plus a **hand-authored** `expected.jl`; presence of `expected.jl` is gate
  membership, and the suite also guards idempotence + clean reparse over *all*
  fixtures), and everything else — CLI, LSP, linter, semantic, index, config —
  in the root `tests/*.rs`.
- `insta` snapshots live in each crate's `tests/snapshots/`. **Never accept a
  snapshot you have not read.**
- Which suite to reach for: formatter bug → a formatter fixture; parser bug → a
  parser fixture + `cargo insta review`; lint rule → a `#[test]` in
  `tests/linter_rules.rs` (no fixture dir) plus the rule's own `examples()`;
  cross-file/LSP work → `tests/lsp.rs` and `tests/salsa_incremental.rs`, which
  guards that a body edit does *not* invalidate the project graph.
- **CI tests on Windows.** Unix-style absolute paths (`/work`, `/abs/c.jl`) are
  **not absolute on Windows**. Any test exercising absolute-path resolution or
  asserting on `file:` URIs must build platform-native paths — see the
  `abs`/`file_uri` helpers in `src/lsp/document_link.rs`'s tests.
- `.github/workflows/smoke-test.yml` runs `fatou debug format` over real Julia
  repos weekly and files one deduped issue per (repo, failure category).

Skills for the recurring workflows: `formatter` (grow the formatter against
hand-authored fixtures), `parser-parity` (close a JuliaSyntax gap),
`add-lint-rule` (the full sequence for a new rule), `linter-investigation`
(triage against a real Julia codebase), `smoke-test-triage`.

## Environment

The dev environment is `devenv`/Nix (`devenv.nix`, `devenv.yaml`): the Rust
toolchain, a Julia interpreter, `go-task`, `mdbook`, `cargo-insta`,
`cargo-audit`, `cargo-deny`, `hyperfine`, and friends, plus the git hooks that
run on commit (clippy, rustfmt, biome).

**Julia packages are Pkg-managed, not Nix-managed**, via the repo's pinned root
`Project.toml` + committed `Manifest.toml`; the shell exports `JULIA_PROJECT=@.`
so every Julia call activates it. Julia is needed only to *regenerate* the
oracle corpus and the generated tables — never to build or test.

Claude Code on the web runs in a container with neither devenv nor Julia, so a
`SessionStart` hook (`.claude/hooks/session-start.sh`) provisions what it can
there; it no-ops outside a remote session. It warns and continues if Julia
cannot be provisioned, because the test suite does not need it.
