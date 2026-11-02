---
layout: home
title: Fatou
hero:
  name: Fatou
  text: Julia tooling, ready when you are.
  tagline: A language server, formatter, and linter for Julia, written in Rust.
  image:
    src: /images/logo.png
    alt: Fatou logo
  actions:
    - theme: brand
      text: Get Started
      link: /guide/getting-started.html
    - theme: alt
      text: Editor Setup
      link: /guide/editors.html
    - theme: alt
      text: View on GitHub
      link: https://github.com/jolars/fatou
features:
  - title: Language server
    details: Completion, diagnostics, navigation, and refactoring powered by incremental analysis.
    link: /guide/editors.html
  - title: Canonical formatting
    details: Consistent Julia formatting for individual files and entire projects.
    link: /guide/formatting.html
  - title: Semantic linting
    details: Find bugs and apply safe fixes without running Julia.
    link: /guide/linting.html
---

<span id="fatou"></span>

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
see [Getting Started](guide/getting-started.md).

## Where to Go Next

- [Getting Started](guide/getting-started.md): complete installation and
  first-run walkthrough.
- [Editor Setup](guide/editors.md): connect the language server to your editor.
- [Configuration](guide/configuration.md): set up `fatou.toml` for your project.
- [Configuration Reference](reference/configuration.md): every `fatou.toml` key.
- [CLI Reference](reference/cli.md): every command and option.
