# Overview

<span id="fatou"></span>

Fatou is a formatter, linter, and language server for
[Julia](https://julialang.org), written in Rust. It keeps formatting consistent,
finds common mistakes, and provides diagnostics and navigation in your editor.

## Built on your code

Fatou parses Julia into a lossless syntax tree and updates its analysis as you
type. The same parser powers the formatter, linter, and language server.
Formatting is deterministic, and formatting an already-formatted file changes
nothing.

Analysis runs without evaluating your code or starting Julia. Fatou reads your
project's package environment statically to resolve names across files and
installed dependencies. In your editor, it also checks `Project.toml` and
`Manifest.toml` and helps you navigate dependencies.

Share formatting settings and lint rules in
[`fatou.toml`](guide/configuration.md), and use the same commands locally and in
[CI](guide/integrations.md).

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
- [Language server](guide/language-server.md): use completion, navigation,
  refactoring, and dependency updates in your editor.
- [Integrations](guide/integrations.md): run checks in CI or a pre-commit hook.

For individual commands, settings, and diagnostics, see the [CLI
reference](reference/cli.md), [Configuration
reference](reference/configuration.md), and [Lint rules](reference/rules.md).
