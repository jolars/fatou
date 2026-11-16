# Contributing

Thanks for your interest in contributing to Fatou! Bug reports, fixes, new lint
rules, parser and formatter work, and documentation improvements are all
welcome. For larger changes, please open an
[issue](https://github.com/jolars/fatou/issues) first so we can discuss the
approach.

## Development environment

The repository ships a [devenv](https://devenv.sh) (Nix) environment
(`devenv.nix`) that provides the pinned Rust toolchain, a Julia interpreter,
Node.js, pnpm, [`task`](https://taskfile.dev), and the auxiliary cargo tools.
Enter it with `devenv shell`. Julia packages (JuliaSyntax.jl, the parser oracle,
plus the formatter-comparison tools) are managed by Julia's own package manager
via the repo's pinned `Project.toml`/`Manifest.toml`, not by Nix.

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
also be dispatched manually. It uses nightly Julia and an isolated checkout of
Julia's development branch to regenerate both parser corpora, produce parity
reports, and run the workspace tests. It opens or updates one PR on
`automation/juliasyntax-oracle`; failed validation leaves a draft PR and fails
the workflow. Review changed expectations and fix parser regressions before
merging. The job never weakens the allowlists or blocked lists. The first PR
proposes the migration from the released JuliaSyntax oracle.

To generate a candidate locally, enter devenv and run:

```sh
bash scripts/refresh-juliasyntax-oracle.sh
```

Pass a Julia commit as the sole argument to replay a source revision. The
generated `.juliasyntax-source` records that commit, the interpreter version and
commit, and the requested syntax version. Use the recorded interpreter when
reproducing an update. The syntax version comes from the source checkout's
`VERSION` file, so a compatible stable interpreter can also exercise development
grammar. The root `Project.toml` and `Manifest.toml` are unaffected. Changes to
the parser's Unicode tables still require the table generators and their usual
review before adopting a new oracle.

After a development update is merged, the usual
`scripts/update-juliasyntax-corpus.sh` command replays the recorded upstream
revision rather than loading the released package from the root environment.

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
