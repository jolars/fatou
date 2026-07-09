# Fatou <img src='https://raw.githubusercontent.com/jolars/fatou/main/assets/logo.png' align="right" width="139" />

[![Build and
Test](https://github.com/jolars/fatou/actions/workflows/build-and-test.yml/badge.svg?branch=main)](https://github.com/jolars/fatou/actions/workflows/build-and-test.yml)
[![Lint](https://github.com/jolars/fatou/actions/workflows/lint.yml/badge.svg?branch=main)](https://github.com/jolars/fatou/actions/workflows/lint.yml)
[![Documentation](https://github.com/jolars/fatou/actions/workflows/docs.yml/badge.svg?branch=main)](https://fatou.dev/)
[![Open
VSX](https://img.shields.io/open-vsx/v/jolars/fatou?logo=vsix)](https://open-vsx.org/extension/jolars/fatou)
[![VS
Code](https://vsmarketplacebadges.dev/version-short/jolars.fatou.svg?logo=vsix)](https://marketplace.visualstudio.com/items?itemName=jolars.fatou)

A language server, formatter, and linter for [Julia](https://julialang.org) that
doesn't require running Julia itself. Fatou is written in Rust and is designed
to be fast, safe, and easy to integrate into editors and tooling. It is named
after the French mathematician Pierre Fatou, whose Fatou set is the complement
of the Julia set.

Fatou follows the rust-analyzer design (a lossless
[`rowan`](https://crates.io/crates/rowan) CST,
[`salsa`](https://crates.io/crates/salsa) for incremental computation, and
[`lsp-server`](https://crates.io/crates/lsp-server) for the language-server
transport).

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
