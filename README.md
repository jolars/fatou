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

**Fatou** is a language server, formatter, and linter for
[Julia](https://julialang.org) that never has to run Julia itself.

It parses Julia once and serves three tools from that tree:

- **Formatter** (`fatou format`): deterministic, rule-based layout.
- **Linter** (`fatou lint`): diagnostics with source snippets.
- **Language server** (`fatou lsp`): both, live in your editor.

Written in Rust, Fatou is fast, safe, and easy to embed in editors and tooling.
The architecture follows [rust-analyzer](https://rust-analyzer.github.io/): a
lossless [`rowan`](https://crates.io/crates/rowan) CST that reconstructs the
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
your platform, you can use the installer scripts below. These scripts are
fetched directly from this repository and then download the latest matching
Fatou release asset for your platform, installing to a user-local directory by
default. If you prefer, download and inspect the script before running it.

For macOS and Linux:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
    https://github.com/jolars/fatou/releases/latest/download/fatou-installer.sh | sh
```

For Windows PowerShell:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://github.com/jolars/fatou/releases/latest/download/fatou-installer.ps1 | iex"
```

## Usage

```sh
# Print the CST (reads stdin if no file)
fatou parse file.jl

# Format to stdout (reads stdin if omitted)
fatou format file.jl

# Verify formatting without writing—exits non-zero if anything would change
fatou format --check src/

# Lint (exits non-zero if there are any findings)
fatou lint src/

# Run the language server over stdio
fatou lsp
```

Configuration lives in `fatou.toml`: `[format]` sets line and indent widths,
`[lint]` selects or ignores rules. Fatou walks up from the file's directory to
find one, stopping at the repository root, then falls back to `$FATOU_CONFIG`
and to a global user config at `~/.config/fatou/fatou.toml`.

## Editor Integration

The language server (`fatou lsp`) runs over stdio and provides a broad set of
features: completion (including the REPL's LaTeX and emoji input sequences, so
`\alpha` inserts `α`), hover, go-to-definition, find references and document
highlights, rename, document and workspace symbols, call and type hierarchy,
signature help, code actions, folding and selection ranges, document links, and
semantic tokens, alongside formatting (whole-document and range) and diagnostics
(push and pull).

The **Fatou** extension for VS Code and Open VSX
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

The parser and formatter are published as standalone, wasm-compatible crates
for embedding in other tools:

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
