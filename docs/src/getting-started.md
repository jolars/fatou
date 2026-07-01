# Getting Started

## Installation

Fatou runs on Linux, macOS, and Windows (x86_64 and arm64), and is available
from several sources.

### Cargo

Install from [crates.io](https://crates.io/crates/fatou) with Cargo:

```bash
cargo install fatou
```

### npm

The `fatou-cli` package bundles a prebuilt binary:

```bash
npm install -g fatou-cli
```

### PyPI

Install the binary as a Python tool:

```bash
uv tool install fatou
# or
pipx install fatou
```

### Prebuilt binaries

Download an archive for your platform from the [releases
page](https://github.com/jolars/fatou/releases) and put the `fatou` binary on
your `PATH`.

### From source

Clone the repository and build a release binary:

```bash
git clone https://github.com/jolars/fatou
cd fatou
cargo build --release
```

The binary is written to `target/release/fatou`.

## First Run

Format a file in place:

```bash
fatou format file.jl
```

Check formatting without writing changes (prints a diff, exits non-zero if any
file would change):

```bash
fatou format --check file.jl
```

Lint a file (or pipe from stdin):

```bash
fatou lint --check file.jl
```

Run the language server over stdio (for editor integration):

```bash
fatou lsp
```

See the [CLI Reference](reference/cli.md) for the full set of commands and
options, and [Editor Setup](guide/editors.md) to wire the language server into
your editor.
