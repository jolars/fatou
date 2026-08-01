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

**Scope (see `TODO.md`):** Fatou has a working parser, formatter, linter, and a
broad language-server surface (see **Language server** below). `TODO.md` is the
live roadmap for remaining work and records known issues and follow-ups; when in
doubt about scope or priority, it is the source of truth.

The dev environment is provided via `devenv`/Nix (`devenv.nix`, `devenv.yaml`)
and includes a Julia toolchain.

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
gate is **hand-authored fixtures**: `tests/fixtures/formatter/<slug>/` holds an
`input.jl` and a hand-written `expected.jl`, and `tests/formatter.rs` asserts
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
(`src/parser/sexpr.rs`, also `fatou parse --to sexpr`) walks the CST and emits
JuliaSyntax's s-expression shape; the harness (`tests/juliasyntax_oracle.rs`)
diffs each fixture against a pinned `expected.sexpr` and gates regressions via
allowlists (no Julia needed at test time → CI-safe). A curated dir corpus
(`tests/fixtures/oracle/`) and a harvested JuliaSyntax sub-corpus
(`juliasyntax.jsonl`) feed it. **To grow parser parity against the oracle, use
the `parser-parity` skill** (`.claude/skills/parser-parity/`). It documents the
loop (probe → grammar + projector → fixture → re-triage → allowlist) and keeps a
rolling `RECAP.md`. See `TODO.md` for the current standing and backlog.

Regenerating the pinned corpus (`scripts/update-juliasyntax-corpus.sh`) is the
one task that does need Julia, at the exact versions recorded in
`tests/fixtures/oracle/.juliasyntax-source` — a different Julia or JuliaSyntax
rewrites unrelated fixtures and buries the intended change. Re-running the
script should leave every file it did not target byte-identical; treat any
other diff as a version mismatch, not a parser change.

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
cargo run -- lint <dir>                      # lint; non-zero if any findings
cargo run -- lsp                             # run the language server on stdio
```

Snapshot tests use `insta`: review/accept with
`cargo insta review`/`cargo insta accept`. Logging honors `RUST_LOG` (e.g.
`RUST_LOG=debug`) via `env_logger`. `task <name>` (Taskfile.yml) wraps the
common workflows.

## Architecture

**Parse pipeline** (`src/parser/`, public API `parse`/`reconstruct` re-exported
from `src/parser.rs`): a lossless `rowan` CST built via an event-based pipeline.

```
lex (lexer.rs) → Vec<Token>
parse_expr (expr.rs, Pratt) + structural.rs (recursive descent) → Vec<Event>
build_tree (tree_builder.rs) → rowan SyntaxNode (CST)
```

- `core.rs` drives the loop; `events.rs` defines `Event` (start node/token/finish
  node); `cursor.rs`, `context.rs`, `diagnostics.rs`, `recovery.rs`
  support the parser. `src/syntax.rs` defines `SyntaxKind` (rowan-style
  `SCREAMING_SNAKE_CASE`) and the `JuliaLanguage` binding.
- **Losslessness is the core invariant:** all whitespace, newlines, and comments
  (including nested `#= =#`) are preserved; `reconstruct(text) == text`. The
  grammar is a deliberately small **walking skeleton** (literals, operators with
  Julia precedence, calls, indexing, and the `function`/`if`/`begin` block
  forms) and grows incrementally (`TODO.md`). Unlike R, Julia has no `[[`/`]]`
  bracket ambiguity, so there is no bracket-rebalancer pass.
- `src/ast/` is the typed AST interface over the CST (see **AST wrappers**
  below).
- `src/incremental.rs` models file text → CST as a `salsa` query
  (`parsed_document`). The token/block reparse *splicing* is deferred; today a
  text edit triggers a full parse (still correct).

**AST wrappers** (`src/ast/`, re-exported from `src/ast.rs`): the typed interface
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
relevant `Has*` trait, re-export from `src/ast.rs`, and add an accessor unit test.

**Formatter** (`src/formatter/`, public API in `src/formatter.rs`): consumes the
CST and uses a Wadler/Prettier-style document IR (`ir.rs`) printed by a single
best-fit layout engine (`printer.rs`) that makes all line-break decisions.
`style.rs` is `FormatStyle`; `check.rs` exposes `check_paths`. Fatou owns its
style (no external reference formatter). `rules::lower` (`rules.rs`) walks the CST
into IR; constructs with a rule are reshaped and everything else is lowered
*transparently* (verbatim tokens, recurse into children), so unhandled syntax
stays byte-identical and the pass stays idempotent while rules land incrementally.
Hand-authored fixtures (`tests/formatter.rs`) gate the output; grow them with the
`formatter` skill.

**Linter** (`src/linter/`): `check_paths` parses each file and reports
`LintStatus` (`Clean`/`Findings`/`ParseDiagnostics`); parse diagnostics
block linting a file. The `Rule` trait + registry (`rules.rs`, `all_rules()` is
the single source of truth), `# fatou-ignore` suppression (`suppression.rs`),
diagnostics + autofixes (`diagnostic.rs`, `fix.rs`), and rendering
(`render.rs`) are in place, with the first rules shipped. The rule roadmap
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

- Integration tests in `tests/*.rs`; fixtures in
  `tests/fixtures/{parser,formatter}/<case>/`. Parser fixtures hold `input.jl`
  (snapshot the CST + diagnostics, assert losslessness); formatter fixtures hold
  `input.jl` + a hand-authored `expected.jl` (the gate in `tests/formatter.rs`,
  which also guards idempotence + clean reparse over all fixtures).
- `insta` snapshots live in `tests/snapshots/`.
- `tests/lsp.rs` drives the language server over an in-memory connection.
- **CI tests on Windows too.** Unix-style absolute paths (`/work`, `/abs/c.jl`)
  are **not absolute on Windows**: `is_absolute()` is false without a drive
  letter, and `std::path::absolute` grafts the CWD's drive onto driveless
  paths. Any test that exercises absolute-path resolution or asserts on `file:`
  URIs must build platform-native paths — see the `abs`/`file_uri` helpers in
  the `src/lsp/document_link.rs` tests. Paths that stay relative-joined and are
  never asserted on verbatim are fine as-is.
