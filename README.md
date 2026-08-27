# Fatou <img src='https://raw.githubusercontent.com/jolars/fatou/main/assets/logo.png' align="right" width="139" />

[![Build and
Test](https://github.com/jolars/fatou/actions/workflows/build-and-test.yml/badge.svg?branch=main)](https://github.com/jolars/fatou/actions/workflows/build-and-test.yml)
[![Crates.io](https://img.shields.io/crates/v/fatou.svg?logo=rust)](https://crates.io/crates/fatou)
[![Open
VSX](https://img.shields.io/open-vsx/v/jolars/fatou?logo=vsix)](https://open-vsx.org/extension/jolars/fatou)
[![VS
Code](https://vsmarketplacebadges.dev/version-short/jolars.fatou.svg?logo=vsix)](https://marketplace.visualstudio.com/items?itemName=jolars.fatou)
[![PyPI
version](https://badge.fury.io/py/fatou.svg?icon=si%3Apython)](https://pypi.org/project/fatou/)
[![npm
version](https://badge.fury.io/js/@fatou-cli%2Ffatou-cli.svg?icon=si%3Anpm)](https://www.npmjs.com/package/fatou-cli)

Fatou is a language server, formatter, and linter for
[Julia](https://julialang.org) that never has to run Julia itself. It bundles
three tools in one:

- **Formatter** (`fatou format`): fast, deterministic, and opinionated
- **Linter** (`fatou lint`): configurable, with auto-fix support for many rules
- **Language server** (`fatou lsp`): both of the above, plus IDE features like
  completion, hover, go-to-definition, and more

Fatou is fast, safe, and easy to embed in editors and tooling. The architecture
follows [rust-analyzer](https://rust-analyzer.github.io/): a lossless
[`rowan`](https://crates.io/crates/rowan) syntax tree that reconstructs the
input byte-for-byte, [`salsa`](https://crates.io/crates/salsa) for incremental
recomputation, and [`lsp-server`](https://crates.io/crates/lsp-server) for the
language-server transport. It is named after the French mathematician Pierre
Fatou, whose Fatou set is the complement of the Julia set.

## Installation

Fatou is available from several sources:

- **crates.io**: `cargo install fatou`
- **Homebrew**: `brew install jolars/tap/fatou`
- **npm**: `npm install -g fatou-cli` (bundles a prebuilt binary)
- **PyPI**: `uv tool install fatou`/`pipx install fatou`
- **AUR** (Arch Linux): `paru -S fatou-bin` (or any other AUR helper)
- **NixOS**: the `fatou` package is available in the Nixpkgs repository
- **Prebuilt binaries**: from the [releases
  page](https://github.com/jolars/fatou/releases)
- **VS Code/Open VSX**: the **Fatou** extension
  ([Marketplace](https://marketplace.visualstudio.com/items?itemName=jolars.fatou),
  [Open VSX](https://open-vsx.org/extension/jolars/fatou)); also works in
  Positron

### Install Script

If you prefer a one-liner installer that picks the right release artifact for
your platform, you can use the installer scripts below, which download the
latest matching Fatou release for your platform, installing to a user-local
directory by default. If you prefer, download and inspect the script before
running it.

For macOS and Linux:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://fatou.dev/install | sh
```

For Windows PowerShell:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://fatou.dev/install.ps1 | iex"
```

## Usage

```sh
# Format in place
fatou format file.jl

# Verify formatting without writing
fatou format --check src/

# Lint (exits non-zero if there are any findings)
fatou lint src/

# Fix lint findings in place
fatou lint --fix file.jl
```

Configuration lives in `fatou.toml`: `[format]` sets line and indent widths,
`[lint]` selects or ignores rules. Fatou walks up from the file's directory to
find one, stopping at the repository root, then falls back to `$FATOU_CONFIG`
and to a global user config at `~/.config/fatou/fatou.toml`.

## Editor Integration

The language server (`fatou lsp`) runs over stdio and provides a broad set of
features:

- formatting (whole-document and range),
- linting (push and pull),
- completion (including the REPL's LaTeX and emoji input sequences), hover,
- go-to-definition, -find references,
- document highlights,
- rename (of symbols, and of files and folders),
- document and workspace symbols,
- call and type hierarchy,
- signature help,
- code actions,
- folding and selection ranges,
- document links, and
- semantic tokens.

Static docstrings participate in those features: decoded local and indexed
documentation renders as Markdown, headings appear in the outline and fold as
sections, external and internal Markdown links navigate, explicit Documenter
`@ref` targets complete and jump to definitions, and Julia-declared fences get
completion, hover, signature help, definition, selection, folding, and semantic
highlighting.

The Fatou extension for VS Code and Open VSX
([Marketplace](https://marketplace.visualstudio.com/items?itemName=jolars.fatou),
[Open VSX](https://open-vsx.org/extension/jolars/fatou)) bundles the binary and
starts the server automatically; it also works in Positron. See
[`editors/code`](editors/code) for the extension, or the [editor setup
guide](https://fatou.dev/guide/editors.html) for Neovim and other editors.

## Integrations

Run format and lint checks in GitHub Actions with
[fatou-action](https://github.com/jolars/fatou-action), which installs a
prebuilt, checksum- and provenance-verified binary:

```yaml
- uses: jolars/fatou-action@v1
```

Or as [pre-commit](https://pre-commit.com) hooks with
[fatou-pre-commit](https://github.com/jolars/fatou-pre-commit):

```yaml
repos:
  - repo: https://github.com/jolars/fatou-pre-commit
    # fatou version
    rev: v0.7.0
    hooks:
      - id: fatou-lint
      - id: fatou-format
```

## Library Crates

The parser and formatter are published as standalone, wasm-compatible crates for
embedding in other tools:

- [fatou-parser](https://crates.io/crates/fatou-parser): lossless CST parser,
  typed AST wrappers, and incremental reparser.
- [fatou-formatter](https://crates.io/crates/fatou-formatter): the formatting
  engine, with optional `serde`/`schema` features.

## Documentation

See <https://fatou.dev/> for the full documentation.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md).

## License

MIT, see [LICENSE](LICENSE).
