# Fatou <img src='https://raw.githubusercontent.com/jolars/fatou/main/assets/logo.png' align="right" width="139" />

A language server, formatter, and linter for [Julia](https://julialang.org),
written in Rust.

Fatou follows the rust-analyzer design (a lossless
[`rowan`](https://crates.io/crates/rowan) CST,
[`salsa`](https://crates.io/crates/salsa) for incremental computation, and
[`lsp-server`](https://crates.io/crates/lsp-server) for the language-server
transport) and is modeled directly on the author's R tooling project, `arity`.

> **Status: early groundwork.** The full architecture is in place; the parser
> covers a growing Julia subset, the formatter has started landing per-construct
> layout rules (gated by hand-authored fixtures), and no lint rules ship yet.
> See `TODO.md` for the roadmap and `AGENTS.md` for the design tenets.

## Installation

Fatou is available from several sources:

- **crates.io**: `cargo install fatou`
- **npm**: `npm install -g fatou-cli` (bundles a prebuilt binary)
- **PyPI**: `uv tool install fatou`/`pipx install fatou`
- **Prebuilt binaries**: from the [releases
  page](https://github.com/jolars/fatou/releases)
- **VS Code/Open VSX**: the **Fatou** extension (also works in Positron)

Runs on Linux, macOS, and Windows (x86_64 and arm64).

## Usage

```sh
fatou parse <file.jl>          # print the CST (stdin if no file)
fatou format <file.jl>         # format to stdout (stdin if omitted)
fatou format --check <dir>     # check formatting; non-zero exit if any differ
fatou lint --check <dir>       # lint
fatou lsp                      # run the language server on stdio
```

Configuration lives in `fatou.toml` (`[format]` line_width/indent_width,
`[lint]` select/ignore).

## Editor integration

The language server (`fatou lsp`) provides formatting and parse diagnostics over
stdio. The **Fatou** extension for VS Code/Open VSX (and Positron) bundles the
binary and starts the server automatically; see [`editors/code`](editors/code).
For Neovim and other editors, see the [editor setup
guide](https://fatou.dev/guide/editors.html).

## Development

```sh
cargo build
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt -- --check
```

Or via [`task`](https://taskfile.dev): `task test`, `task lint`, `task format`.

## License

MIT—see [LICENSE](LICENSE).
