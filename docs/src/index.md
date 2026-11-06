---
layout: home
title: Fatou
hero:
  name: Fatou
  text: Julia tooling, minus the runtime.
  tagline: A formatter, linter, and language server for Julia, written in Rust.
    Keep your code consistent and get feedback as you write, without starting Julia.
  image:
    src: /images/logo-hero.png
    alt: Fatou logo
    width: 1200
    height: 630
  actions:
    - theme: brand
      text: Get started
      link: /guide/getting-started.html
    - theme: alt
      text: Editor setup
      link: /guide/editors.html
    - theme: alt
      text: View on GitHub
      link: https://github.com/jolars/fatou
features:
  - title: Format consistently
    details: Apply canonical formatting to individual Julia files or entire projects, and check formatting in CI.
    link: /guide/formatting.html
  - title: Find problems early
    details: Catch common mistakes with semantic lint rules, apply safe fixes, and share checks through project configuration.
    link: /guide/linting.html
  - title: Work in your editor
    details: Get completion, diagnostics, navigation, and refactoring through the language server, with formatting on save.
    link: /guide/editors.html
---

<span id="fatou"></span>

## Quick start

Install Fatou for your platform:

::: code-group

```sh [macOS/Linux]
curl --proto '=https' --tlsv1.2 -sSf https://fatou.dev/install | sh
```

```powershell [Windows]
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://fatou.dev/install.ps1 | iex"
```

:::

See [Installation](installation.md) for Homebrew, npm, PyPI, Cargo, Nix, and
other installation options.

After installation, format and lint a file:

```sh
fatou format file.jl
fatou lint file.jl
```

Using VS Code? Install the [Fatou extension](guide/editors.md#vs-code), which
includes the binary. For Zed, Neovim, Helix, and other editors, see [Editor
setup](guide/editors.md).

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

## Where to go next

- [Getting started](guide/getting-started.md): check and format an existing
  Julia project.
- [Editor setup](guide/editors.md): connect the language server to your editor.
- [Configuration](guide/configuration.md): set up `fatou.toml` for your project.
- [Configuration reference](reference/configuration.md): every `fatou.toml` key.
- [CLI reference](reference/cli.md): every command and option.
