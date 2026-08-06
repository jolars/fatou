# Agent Instructions

This file provides guidance to coding agents when working with code in this
repository.

## Project

Fatou is a Rust CLI providing a language server, formatter, and linter for the
Julia language. Cargo workspace (edition 2024) with the CLI as the root
package (binary and library crate both named `fatou`) and two published
library crates under `crates/`:

- `fatou-parser` — the lossless CST parser, typed AST wrappers, and
  incremental reparser (`syntax`, `ast`, `parser` modules; the root re-exports
  them, so `fatou::parser::…` paths keep working).
- `fatou-formatter` — the formatting engine; the root's `src/formatter.rs` is
  the CLI bridge that re-exports it and hosts the batch `check` API.

Both library crates must stay `wasm32-unknown-unknown`-clean (no filesystem,
process, thread, or clock use) — a dprint plugin embeds the formatter as a
Wasm module. A dedicated CI job enforces this. Release streams are versioned
independently by versionary: the CLI keeps plain `vX.Y.Z` tags; member crates
tag as `fatou-parser-vX.Y.Z`/`fatou-formatter-vX.Y.Z`.

The design follows rust-analyzer (and the author's R tool, `arity`, on which
this is modeled directly):

- lossless `rowan` CST trees,
- `salsa` for the incremental database,
- `lsp-server` for the language-server transport,
- an event-pipeline parser built for incremental reparse.

**Scope (see `TODO.md`):** Fatou has a working parser, formatter, linter, and a
broad language-server surface (see **Language server** below). `TODO.md` is the
live roadmap for remaining work and records known issues and follow-ups; when in
doubt about scope or priority, it is the source of truth.

The dev environment is provided via `devenv`/Nix (`devenv.nix`, `devenv.yaml`)
and includes a Julia interpreter; Julia packages (the JuliaSyntax parser oracle
and formatter-comparison tools) are Pkg-managed via the repo's pinned
`Project.toml`/`Manifest.toml`, not by Nix (see **Parser oracle**).

Claude Code on the web runs in a container with neither, so a `SessionStart`
hook (`.claude/hooks/session-start.sh`) provisions them there; it no-ops
outside a remote session, leaving local machines to `devenv`. Julia is optional
in that container — it is needed only to *regenerate* the oracle corpus (see
below), never to run the tests — so the hook warns and continues if it cannot
be provisioned.

## Tenets

1. **Deterministic, canonical formatting.** Output is decided solely by the
   formatter's rules and the layout engine, never by how the input happens to be
   written. Semantically-equivalent inputs **must** format identically: the
   input's line breaks, whitespace, operator spelling (`in` vs `∈`, `a*b` vs
   `a * b`), and numeric-literal form never influence the result. Fatou does
   **not** honor "persistent line breaks"; it **fully reflows**, laying out each
   construct from scratch under `line_width` and breaking only where width or
   semantics require it, regardless of where the source broke. Push back against
   hard-coding special cases for specific constructs. Honoring "persistent line
   breaks" would make Fatou's output depend on its input, which this tenet
   forbids. Any deviation from full reflow is a deliberate, recorded choice,
   never silent non-determinism.
2. **Incremental parsing is first-class**, not an afterthought. Parser/CST work
   must keep the `salsa`-based reparse path (`src/incremental.rs`) viable.
3. **Parsing is the parser's job.** Never paper over parser mistakes in the
   formatter, and never let parsing logic creep into the formatter. If the
   formatter hits something the parser handled wrong, fix it in the parser.
4. **Losslessness is the parser's job.** The parser preserves all text
   (whitespace, comments, etc.) so that `reconstruct(text) == text` always. The
   formatter can assume the CST is lossless and focus on layout.

A lint fix is not a formatter. `lint --fix` applies each fix as a byte-range
replacement (`apply_fixes`, `src/linter/fix.rs`) and never runs the formatter.
A fix must stay locally legible on its own (don't jam tokens together), but it
need **not** satisfy line width or produce canonical layout; the formatter owns
that. Run `format` afterward if canonical output is wanted.

## Formatter testing

Fatou owns its formatting style; there is **no external reference formatter**.
(We used to track Runic.jl as a soft oracle; that target has been removed.) The
gate is **hand-authored fixtures**: `crates/fatou-formatter/tests/fixtures/formatter/<slug>/` holds an
`input.jl` and a hand-written `expected.jl`, and `crates/fatou-formatter/tests/formatter.rs` asserts
`format(input.jl) == expected.jl`. **Presence of `expected.jl` is gate
membership** — a fixture with only `input.jl` is a construct still being authored.
There is no allowlist or blocked list. A second test
(`formatter_is_idempotent_and_stable`) runs over **every** `input.jl` and checks
`format(format(x)) == format(x)` plus clean reparse of the output.

`expected.jl` is authored under Tenet 1 (deterministic full reflow), never
captured from any formatter. **To grow the formatter, use the `formatter`
skill** (`.claude/skills/formatter/`). It documents the human-in-the-loop loop
(propose → user edits `expected.jl` → push back → implement the rule) and keeps a
rolling `RECAP.md`.

## Parser oracle

The differential oracle for the parser is **JuliaSyntax.jl** (the official Julia
parser, itself a lossless green-tree design). A *projector*
(`crates/fatou-parser/src/parser/sexpr.rs`, also `fatou parse --to sexpr`) walks the CST and emits
JuliaSyntax's s-expression shape; the harness (`crates/fatou-parser/tests/juliasyntax_oracle.rs`)
diffs each fixture against a pinned `expected.sexpr` and gates regressions via
allowlists (no Julia needed at test time → CI-safe). A curated dir corpus
(`crates/fatou-parser/tests/fixtures/oracle/`) and a harvested JuliaSyntax sub-corpus
(`juliasyntax.jsonl`) feed it. **To grow parser parity against the oracle, use
the `parser-parity` skill** (`.claude/skills/parser-parity/`). It documents the
loop (probe → grammar + projector → fixture → re-triage → allowlist) and keeps a
rolling `RECAP.md`. See `TODO.md` for the current standing and backlog.

Regenerating the pinned corpus (`scripts/update-juliasyntax-corpus.sh`) is the
one task that does need Julia. All Julia packages are Pkg-managed via the repo's
root `Project.toml` + committed `Manifest.toml`; nix (`devenv.nix`) provides only
the bare `julia-bin` interpreter, and the devenv shell exports `JULIA_PROJECT=@.`
so every Julia call activates the root project. This replaces nixpkgs'
`withPackages`, which resolved an old registry snapshot and pinned JuliaSyntax by
accident, defeating the oracle's exact-version contract. The regen scripts just
`using JuliaSyntax` from the *active* environment — they must not force-activate
or instantiate the root project, because the web-container `SessionStart` hook
provisions JuliaSyntax differently: a pinned git checkout on `JULIA_LOAD_PATH`,
avoiding the Pkg/registry access that container lacks. JuliaSyntax is pinned
exactly (`=0.4.10`) in `[compat]`; the regen scripts mirror the resolved versions
into `crates/fatou-parser/tests/fixtures/oracle/.juliasyntax-source`. A different Julia or JuliaSyntax
rewrites unrelated fixtures and buries the intended change, so re-running the
script should leave every file it did not target byte-identical; treat any other
diff as a version mismatch, not a parser change. To bump the oracle: edit the
`[compat]` bound in the root `Project.toml`, re-resolve
(`julia --project=. -e 'using Pkg; Pkg.update("JuliaSyntax")'`), then re-run both
regen scripts and re-triage.

## Commands

```sh
cargo build --workspace           # dev build
cargo test --workspace            # all tests
cargo test <substring>            # tests matching a name (root crate)
cargo test -p fatou-parser --test parser_snapshots   # one integration test file
cargo clippy --workspace --all-targets --all-features -- -D warnings   # warnings are errors
cargo fmt --all -- --check        # keep changes rustfmt-clean
```

CLI usage:

```sh
cargo run -- parse <file.jl>                 # print CST; stdin if no file
cat file.jl | cargo run -- parse --verify --quiet   # losslessness round-trip
cargo run -- format <file.jl>                # format to stdout (stdin if omitted)
cargo run -- format --check <dir>            # check without writing; non-zero if any differ
cargo run -- lint <dir>                      # lint; non-zero if any findings
cargo run -- lsp                             # run the language server on stdio
```

Snapshot tests use `insta`: review/accept with
`cargo insta review`/`cargo insta accept`. Logging honors `RUST_LOG` (e.g.
`RUST_LOG=debug`) via `env_logger`. `task <name>` (Taskfile.yml) wraps the
common workflows.

## Architecture

**Parse pipeline** (`crates/fatou-parser/src/parser/`, public API `parse`/`reconstruct` re-exported
from `crates/fatou-parser/src/parser.rs`): a lossless `rowan` CST built via an event-based pipeline.

```
lex (lexer.rs) → Vec<Token>
parse_expr (expr.rs, Pratt) + structural.rs (recursive descent) → Vec<Event>
build_tree (tree_builder.rs) → rowan SyntaxNode (CST)
```

- `core.rs` drives the loop; `events.rs` defines `Event` (start node/token/finish
  node); `cursor.rs`, `context.rs`, `diagnostics.rs`, `recovery.rs`
  support the parser. `crates/fatou-parser/src/syntax.rs` defines `SyntaxKind` (rowan-style
  `SCREAMING_SNAKE_CASE`) and the `JuliaLanguage` binding.
- **Losslessness is the core invariant:** all whitespace, newlines, and comments
  (including nested `#= =#`) are preserved; `reconstruct(text) == text`. The
  grammar is a deliberately small **walking skeleton** (literals, operators with
  Julia precedence, calls, indexing, and the `function`/`if`/`begin` block
  forms) and grows incrementally (`TODO.md`). Unlike R, Julia has no `[[`/`]]`
  bracket ambiguity, so there is no bracket-rebalancer pass.
- `crates/fatou-parser/src/ast/` is the typed AST interface over the CST (see **AST wrappers**
  below).
- `src/incremental.rs` models file text → CST as a `salsa` query
  (`parsed_document`). The token/block reparse *splicing* is deferred; today a
  text edit triggers a full parse (still correct).

**AST wrappers** (`crates/fatou-parser/src/ast/`, re-exported from `crates/fatou-parser/src/ast.rs`): the typed interface
over the rowan CST, modeled on rust-analyzer's `ast` module. Three layers, all
zero-cost newtypes that only cast when a kind matches:

- `nodes.rs` — an `AstNode` newtype per node kind (`FunctionDef`, `CallExpr`, ...)
  with typed accessors (`BinaryExpr::lhs/rhs/op`, `CallExpr::callee`,
  `ArgList::args`, `IfExpr::then_body/elseif_clauses/else_clause`,
  `Condition::expr` which peels one paren layer), plus the `Expr` expression sum.
  `Expr` has a variant per wrapped expression kind and an `Other(SyntaxNode)`
  catch-all, so it stays *total* over expression kinds as the grammar grows — an
  operand of a not-yet-wrapped kind round-trips through `Other` rather than
  vanishing.
- `tokens.rs` — the `AstToken` trait (rowan ships only `AstNode`) and typed token
  newtypes (`Ident`, `Operator`; `Operator::can_cast` is `SyntaxKind::is_operator`,
  the one shared operator predicate). `child_token::<T>` is the typed-token analogue
  of `support::child`.
- `traits.rs` — the `Has*` shape traits (`HasArgList`, `HasBody`, `HasCondition`)
  shared across wrappers, so `arg_list()`/`body()`/`condition()` are one contract.

**Who uses it:** the linter and code actions/fixes, the semantic builder, the LSP
handlers, and `project.rs` — navigate the tree through these wrappers rather than
raw `children()`/`kind()` matching. **Who doesn't:** the **formatter** is
deliberately exempt (it lowers known kinds and recurses over everything else
verbatim — the transparent fallback that guards losslessness/idempotence — so it
works the raw CST directly), and the polymorphic kind-classification walkers
(`lsp/symbols.rs`, `lsp/folding.rs`, `lsp/semantic_tokens.rs`) that dispatch a
single node over many kinds, where single-kind wrappers would add code, not
remove it.

**To grow it:** add the `ast_node!`/`ast_token!` entry, add accessors via
`support::child`/`support::children`/`support::token`/`child_token`, impl any
relevant `Has*` trait, re-export from `crates/fatou-parser/src/ast.rs`, and add an accessor unit test.

**Formatter** (engine in `crates/fatou-formatter/src/formatter/`; the root's
`src/formatter.rs` is the CLI bridge re-exporting it): consumes the CST and
uses a Wadler/Prettier-style document IR (`ir.rs`) printed by a single
best-fit layout engine (`printer.rs`) that makes all line-break decisions.
`style.rs` is `FormatStyle`; the bridge-side `src/formatter/check.rs` exposes
`check_paths` (file walking and rayon stay out of the engine). Fatou owns its
style (no external reference formatter). `rules::lower` (`rules.rs`) walks the CST
into IR; constructs with a rule are reshaped and everything else is lowered
*transparently* (verbatim tokens, recurse into children), so unhandled syntax
stays byte-identical and the pass stays idempotent while rules land incrementally.
Hand-authored fixtures (`crates/fatou-formatter/tests/formatter.rs`) gate the output; grow them with the
`formatter` skill.

**Linter** (`src/linter/`): `check_paths` parses each file and reports
`LintStatus` (`Clean`/`Findings`/`ParseDiagnostics`); parse diagnostics
block linting a file. The `Rule` trait + registry (`rules.rs`, `all_rules()` is
the single source of truth), `# fatou-ignore` suppression (`suppression.rs`),
diagnostics + autofixes (`diagnostic.rs`, `fix.rs`), and rendering
(`render.rs`) are in place, with the first rules shipped. Shared rule
machinery lives on `RuleContext` (the memoized `resolver()`/`file_scan()`/
`resolves_to_base()`/`control_flow()` answers) and in `rules/matchers.rs`
(call-shape matching: `plain_call`, `CallShape`) — reach for those rather than
hand-rolling. The rule roadmap
lives in `TODO.md` ("Rule roadmap"); **to add a rule, use the `add-lint-rule`
skill** (`.claude/skills/add-lint-rule/`).

**Language server** (`src/lsp.rs`, `src/lsp/`, CLI `fatou lsp`): a stdio
JSON-RPC server on the `lsp-server` crate (rust-analyzer's transport). Advertised
capabilities (`server.rs::server_capabilities`, negotiated against the client's)
include completion, hover, definition, references + document highlight, rename
(with prepare), document + workspace symbols, call hierarchy + type hierarchy,
signature help, code actions, folding + selection ranges, document links,
semantic tokens, whole-document + range formatting, and diagnostics (push and,
when the client supports it, pull), plus workspace-folder/file-watch
registration and UTF-8/UTF-16 position-encoding negotiation. Concurrency follows
the rust-analyzer model forced by salsa's single-writer constraint: a dedicated
**analysis thread** (`analysis_thread.rs`) is the sole db owner/writer, splitting
each analysis into a cheap `&mut db` write-phase and a read-phase dispatched to a
fixed **read pool** (`task_pool.rs`, `read_jobs.rs`) holding a short-lived db
clone; requests are coalesced per URI and cancelable, with a stale-read protocol
so a superseded edit's results are dropped. Per-job `catch_unwind` on both the
analysis thread and the read pool keeps one malformed request from killing the
server. Background package indexing gets its own pool (`TODO.md`).

**File discovery** (`src/file_discovery.rs`): `collect_julia_files` walks paths
for `.jl` files (via `ignore`); rejects non-`.jl` explicit file paths.

**Config** (`src/config.rs`): `fatou.toml` with `[format]` (line_width,
indent_width), `[lint]` (select, ignore, severity), and `[julia]` (version — the
target Julia range for the `julia-version-compat` rule). Defaults follow Julia
conventions (width 92, indent 4).

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
- **Never edit the changelog by hand**—`versionary` generates it.

## Testing layout

- Integration tests live with their crate: parser suites and fixtures in
  `crates/fatou-parser/tests/` (`fixtures/parser/<case>/` holds `input.jl`;
  snapshot the CST + diagnostics, assert losslessness), the formatter gate in
  `crates/fatou-formatter/tests/` (`fixtures/formatter/<case>/` holds
  `input.jl` + a hand-authored `expected.jl`; the gate also guards idempotence
  + clean reparse over all fixtures), and CLI/LSP/linter/semantic suites in the
  root `tests/*.rs`.
- `insta` snapshots live in each crate's `tests/snapshots/` (parser snapshots
  with `fatou-parser`, `cfg`/`rule_docs` snapshots at the root).
- `tests/lsp.rs` drives the language server over an in-memory connection.
- **CI tests on Windows too.** Unix-style absolute paths (`/work`, `/abs/c.jl`)
  are **not absolute on Windows**: `is_absolute()` is false without a drive
  letter, and `std::path::absolute` grafts the CWD's drive onto driveless
  paths. Any test that exercises absolute-path resolution or asserts on `file:`
  URIs must build platform-native paths — see the `abs`/`file_uri` helpers in
  the `src/lsp/document_link.rs` tests. Paths that stay relative-joined and are
  never asserted on verbatim are fine as-is.
