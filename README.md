# Fatou <img src='https://raw.githubusercontent.com/jolars/fatou/main/assets/logo.png' align="right" width="139" />

[![CI](https://img.shields.io/github/actions/workflow/status/jolars/fatou/build-and-test.yml?branch=main&label=CI)](https://github.com/jolars/fatou/actions/workflows/build-and-test.yml)
[![Crates.io](https://img.shields.io/crates/v/fatou.svg?logo=rust)](https://crates.io/crates/fatou)
[![Open
VSX](https://img.shields.io/open-vsx/v/jolars/fatou?logo=vsix)](https://open-vsx.org/extension/jolars/fatou)
[![VS
Code](https://vsmarketplacebadges.dev/version-short/jolars.fatou.svg?logo=vsix)](https://marketplace.visualstudio.com/items?itemName=jolars.fatou)
[![PyPi](https://img.shields.io/pypi/v/fatou?logo=python&label=PyPi)](https://pypi.org/project/fatou/)
[![npm](https://img.shields.io/npm/v/fatou-cli?logo=npm&label=npm)](https://www.npmjs.com/package/fatou-cli)

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
- **mise/Aqua**: see the [installation
  guide](https://fatou.dev/guide/getting-started.html#mise-and-aqua)
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
fatou format file.jl
fatou lint file.jl
```

Formatting rewrites the file in place; linting reports findings and exits
nonzero if it finds any. See
[Formatting](https://fatou.dev/guide/formatting.html) for directory runs,
checks, and preservation verification, and
[Linting](https://fatou.dev/guide/linting.html) for rule selection, fixes, and
suppressions.

Put shared settings in `fatou.toml`; the [configuration
guide](https://fatou.dev/guide/configuration.html) explains setup and discovery.

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

Fatou is also available from Zed's extension registry. Install **Julia**, then
**Fatou**, from Zed's extensions view. See the [Zed setup
guide](https://fatou.dev/guide/editors.html#zed) for configuration.

## Integrations

Format Julia files with [dprint](https://dprint.dev) using
[`dprint-plugin-fatou`](https://github.com/jolars/dprint-plugin-fatou). The
plugin bundles the formatter and uses the `fatou` settings in `dprint.json`. See
the [integrations guide](https://fatou.dev/guide/integrations.html#dprint) for
setup, configuration, and updates.

Run format and lint checks in GitHub Actions with
[fatou-action](https://github.com/jolars/fatou-action), which installs a
prebuilt, checksum- and provenance-verified binary. See the [GitHub Actions
guide](https://fatou.dev/guide/integrations.html#github-actions) for a complete
workflow and configuration options.

Run Fatou through [pre-commit](https://pre-commit.com) hooks with
[fatou-pre-commit](https://github.com/jolars/fatou-pre-commit). See the
[pre-commit guide](https://fatou.dev/guide/integrations.html#pre-commit) for
installation, autofixes, and updates.

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
