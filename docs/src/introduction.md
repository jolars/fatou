# Overview

<span id="fatou"></span>

Fatou is a formatter, linter, and language server for
[Julia](https://julialang.org), written in Rust. It keeps formatting consistent,
finds common mistakes, and provides diagnostics and navigation in your editor.
It also checks Julia project and manifest files and helps you navigate package
dependencies. Analysis runs without evaluating your code or starting Julia.

## Choose your setup {#quick-start}

- **VS Code:** install the [Fatou extension](guide/editors.md#vs-code). It
  includes the binary and starts the language server for you. The same extension
  is available for VSCodium and other Code OSS editors.
- **Zed:** install the [Fatou extension](guide/editors.md#zed), which can use an
  installed binary or download one for your platform.
- **Other editors:** follow [Editor setup](guide/editors.md) to connect your
  editor to the language server. The guide includes Neovim, Helix, and Vim.
- **Terminal or CI:** [install the CLI](installation.md), then follow [Getting
  started](guide/getting-started.md) to check and format an existing Julia
  project.

The [Installation](installation.md) page covers the standalone installer,
package managers, source builds, and the dprint plugin.

## Where to go next

- [Getting started](guide/getting-started.md): a workflow for an existing Julia
  project.
- [Formatting](guide/formatting.md) and [Linting](guide/linting.md): format
  code, choose checks, and apply fixes.
- [Configuration](guide/configuration.md): share project settings in
  `fatou.toml`.
- [Integrations](guide/integrations.md): run checks in CI or a pre-commit hook.

For individual commands, settings, and diagnostics, see the [CLI
reference](reference/cli.md), [Configuration
reference](reference/configuration.md), and [Lint rules](reference/rules.md).
