# Contributing

Thanks for your interest in contributing to Fatou! Bug reports, fixes, new lint
rules, parser and formatter work, and documentation improvements are all
welcome. For larger changes, please open an
[issue](https://github.com/jolars/fatou/issues) first so we can discuss the
approach.

## Development environment

The repository ships a [devenv](https://devenv.sh) (Nix) environment
(`devenv.nix`) that provides the pinned Rust toolchain, Julia, Node.js, pnpm,
[`task`](https://taskfile.dev), and the auxiliary cargo tools. On Linux x86-64,
`julia` uses the development build in `scripts/julia-oracle.lock.json`, matching
the oracle runner. Nix patches its ELF loader so package precompilation also
works on NixOS. Other platforms use the released Julia fallback until the lock
includes their binaries.

Enter with `devenv shell`; re-enter after changing the interpreter pin. The root
`Project.toml`/`Manifest.toml` manages the Julia packages for comparison and
asset tools through Julia's package manager. The parser oracle loads its
matching JuliaSyntax checkout separately.

Nix is not required, though. Rust 1.89 or newer is enough to build Fatou; the
development toolchain tracks stable through `rust-toolchain.toml`:

```sh
cargo build --workspace
cargo test --workspace
```

## Quality gates

CI is the source of truth for quality gates. Before opening a pull request, make
sure these pass locally:

```sh
cargo test --workspace                                                 # all tests
cargo clippy --workspace --all-targets --all-features -- -D warnings   # warnings are errors
cargo fmt --all -- --check                                             # rustfmt-clean
```

Or via `task`: `task test`, `task lint`, `task format-check`. CI additionally
runs `cargo-audit` and `cargo-deny`, and builds and tests on Linux, macOS, and
Windows.

`fatou.schema.json` is generated from the configuration types. After changing a
configuration key, type, enum, or default, regenerate it with
`UPDATE_EXPECTED=1 cargo test config_schema`, review the diff, and rerun
`cargo test config_schema`. Do not edit the schema by hand.

The `fatou-parser` reparse benchmark sits behind a `bench` feature, so a plain
`--all-targets` build never pulls in criterion, whose `alloca` dependency wants
a C toolchain. Run it with `task bench-reparse`. The clippy line above passes
`--all-features`, so the bench is still linted.

Snapshot tests use [insta](https://insta.rs): review changed snapshots with
`cargo insta review` and accept them with `cargo insta accept`. Logging in tests
honors `RUST_LOG` (e.g., `RUST_LOG=debug cargo test`).

## Test-driven development

Fatou is developed test-first: write a failing test, watch it fail, then make it
pass. For a bug, add a failing fixture or snapshot that reproduces it before the
fix.

- **Parser fixtures** live in
  `crates/fatou-parser/tests/fixtures/parser/<case>/` with an `input.jl`; the
  harness snapshots the CST and diagnostics and asserts losslessness
  (`reconstruct(text) == text`).
- **Formatter fixtures** live in
  `crates/fatou-formatter/tests/fixtures/formatter/<case>/` with an `input.jl`
  and a hand-authored `expected.jl`. Fatou owns its formatting style;
  `expected.jl` is written by hand, never captured from a formatter. The suite
  also checks idempotence (`format(format(x)) == format(x)`) and clean reparse
  of the output.
- **Parser parity** is measured against JuliaSyntax.jl via a differential oracle
  (`crates/fatou-parser/tests/juliasyntax_oracle.rs`) that needs no Julia at
  test time.

### Development JuliaSyntax oracle

JuliaSyntax development for Julia 1.14 onward lives in the [`JuliaSyntax/`
directory of Julia's
repository](https://github.com/JuliaLang/julia/tree/master/JuliaSyntax). The
**Update JuliaSyntax oracle** workflow runs every Monday at 05:30 UTC and can
also be dispatched manually. Its discovery step resolves a nightly build into
`scripts/julia-oracle.lock.json`, which pins its exact version, full Julia
commit, commit-specific archive URL, and SHA-256. Regeneration verifies that
archive and runtime, then loads JuliaSyntax from the same commit. The workflow
proposes the lock and both refreshed parser corpora in one PR on
`automation/juliasyntax-oracle`; failed validation leaves a draft PR and fails
the workflow. It attaches parity reports and runs the workspace tests. Review
changed expectations and fix parser regressions before merging. The job never
weakens the allowlists or blocked lists. The committed corpora use JuliaSyntax 2
from the locked development tree, with an explicit syntax version recorded in
their provenance. The root Julia project's released JuliaSyntax dependency is
separate from this oracle.

The pinned runner targets Linux x86-64 and requires Python 3.12 or newer and
Git. The devenv shell also supplies the ELF loader needed on NixOS. To discover
and generate a new candidate, run:

```sh
bash scripts/refresh-juliasyntax-oracle.sh --update
```

Without `--update`, that command replays the committed lock without resolving
nightly or `master`. Restore the lock from the same Git revision as the corpus
to replay an older update. The generated `.juliasyntax-source` records the
runtime and source commit, requested syntax version, and verified archive hash.
The refresh also regenerates the Unicode operator table with that toolchain.
Review its diff along with the corpora. Identifier tables still require their
separate generator when Julia's Unicode version changes.

The usual `scripts/update-juliasyntax-corpus.sh` command replays the recorded
upstream revision and locked binary, including the harvested cases and operator
table. Inputs that upstream removes from its harvested test binding belong in
`crates/fatou-parser/tests/fixtures/oracle/retained-inputs.toml`, so
regeneration preserves their regression coverage.

Both corpora require complete triage: every input belongs to its allowlist or
blocked list, and a resolved blocked case must return to the allowlist. Keep
encoding changes separate from grammar and recovery gaps. A curated case moved
to blocked must retain identical input in the parser fixture suite, which guards
its CST, diagnostics, and losslessness independently of the new oracle.

Verified archives are cached under `$XDG_CACHE_HOME/fatou/julia-oracle` (default
`~/.cache/fatou/julia-oracle`); `FATOU_JULIA_ORACLE_CACHE` overrides that
location. The workflow retains a `julia-oracle-binary` artifact for 90 days
because upstream nightly archives expire. To seed the cache from a retained
copy:

```sh
python3 scripts/julia-oracle.py install --archive /path/to/archive.tar.gz
```

The installer rejects checksum, runtime-version, and commit mismatches. If no
retained binary is available, clone Julia, check out the lock's full commit, and
[build that
revision](https://github.com/JuliaLang/julia/blob/master/doc/src/devdocs/build/build.md).
You can explicitly select the resulting interpreter:

```sh
FATOU_ORACLE_JULIA=/path/to/julia/usr/bin/julia bash scripts/refresh-juliasyntax-oracle.sh
```

This fallback checks the locked runtime version and commit and records
`source-build` provenance. It does not claim to reproduce the downloaded
archive's bytes; compare its generated fixtures before using the results.

The workflow uses the existing `RELEASE_TOKEN` when available so its PR triggers
the normal CI workflows. Its `GITHUB_TOKEN` fallback runs validation in the
refresh job itself; GitHub must allow Actions to create pull requests in the
repository settings.

## Architecture and roadmap

- [`AGENTS.md`](https://github.com/jolars/fatou/blob/main/AGENTS.md) documents
  the architecture, the design tenets (deterministic full-reflow formatting,
  first-class incremental parsing, losslessness), and the project conventions in
  detail.
- [`TODO.md`](https://github.com/jolars/fatou/blob/main/TODO.md) is the live
  roadmap and records known issues and follow-ups. When in doubt about scope or
  priority, check there.

## Documentation

The documentation site ([fatou.dev](https://fatou.dev)) uses
[VitePress](https://vitepress.dev/) and Cloudflare Workers. Install its locked
Node dependencies, then preview it locally:

```sh
pnpm --dir docs install --frozen-lockfile
task docs-preview
```

`task docs-build` generates the rule reference and benchmark charts, builds the
site, and checks links, historical URLs and headings, Markdown exports, and
public downloads. It also builds the playground's Wasm bindings with
`wasm-pack`, which the devenv shell provides alongside the
`wasm32-unknown-unknown` Rust target. It reads the committed benchmark artifacts
without running measurements. The output is `docs/.vitepress/dist/`.

Both `task docs-preview` and `task docs-build` rebuild the playground bindings.
After changing Rust code while the development server is running, run
`pnpm --dir docs playground:wasm` and reload the playground. Run
`pnpm --dir docs test:wasm` after a site build to test the generated JavaScript
and Wasm together.

Run `task docs-worker` after building to preview the actual Worker, then run
`task docs-check` in a second terminal to check Markdown negotiation, redirects,
404 responses, installers, and the schema. On NixOS, the devenv shell supplies
the dynamic loader for Cloudflare's local runtime.

Add pages under `docs/src/` and navigation entries in
`docs/.vitepress/config.mjs`. Keep static assets in `docs/src/public/`.
`docs/README.md` describes deployment and the initial hosting cutover.

## Commits and versioning

- Use [Conventional Commits](https://www.conventionalcommits.org)
  (`type(scope): subject`) in the imperative mood, with subject lines ideally
  under 60 characters.
- Fatou follows [semantic versioning](https://semver.org). Releases and
  `CHANGELOG.md` are generated by tooling—never edit the changelog by hand.

## License

By contributing, you agree that your contributions are licensed under the [MIT
License](https://github.com/jolars/fatou/blob/main/LICENSE).
