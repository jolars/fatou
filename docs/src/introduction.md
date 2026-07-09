# Fatou

<img src="./images/logo-small.png" alt="Fatou logo" class="right" style="width: 139px; padding-left: 10px; padding-bottom: 10px;" />

Fatou is a language server, formatter, and linter for the
[Julia](https://julialang.org) language, written in Rust. It follows the
rust-analyzer design (a lossless [`rowan`](https://crates.io/crates/rowan) CST,
[`salsa`](https://crates.io/crates/salsa) for incremental computation, and
[`lsp-server`](https://crates.io/crates/lsp-server) for the language-server
transport).

## Quick Start

Install with Cargo:

```bash
cargo install fatou
```

Format your first file:

```bash
fatou format file.jl
```

For full installation options (npm, PyPI, prebuilt binaries, and source builds),
see [Getting Started](getting-started.md).

## Where to Go Next

- [Getting Started](getting-started.md): complete installation and first-run
  walkthrough.
- [Editor Setup](guide/editors.md): connect the language server to your editor.
- [Configuration](reference/configuration.md): every `fatou.toml` key.
- [CLI Reference](reference/cli.md): every command and option.
