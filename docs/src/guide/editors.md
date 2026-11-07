# Editor setup

Choose your editor below to connect it to Fatou. Once connected, you can format
Julia code, see diagnostics, and navigate your project.

<span id="unicode-symbol-input"></span>

The [language server guide](language-server.md) explains the features you can
use, including refactoring, dependency updates, and [Unicode symbol
input](language-server.md#unicode-symbol-input).

## Prerequisites

The VS Code extension includes a Fatou binary, and the Zed extension can
download one for you. For other setups, [install Fatou](../installation.md)
first. Make sure `fatou` is on your `PATH`, or use its absolute path in your
editor's configuration.

## VS Code

Install the [Fatou
extension](https://marketplace.visualstudio.com/items?itemName=jolars.fatou)
(`jolars.fatou`) from the Marketplace, or from the command line:

```bash
code --install-extension jolars.fatou
```

The extension starts `fatou lsp` when you open a Julia file and registers Fatou
as the default Julia formatter. It includes a matching binary for supported
platforms and downloads one from GitHub releases when none is bundled.

To format on save, add to `settings.json`:

```json
{
  "[julia]": {
    "editor.defaultFormatter": "jolars.fatou",
    "editor.formatOnSave": true
  }
}
```

### Use an installed binary

To use the `fatou` on your `PATH`, add:

```json
{
  "fatou.executableStrategy": "environment"
}
```

To use a binary at a specific path, add:

```json
{
  "fatou.executableStrategy": "path",
  "fatou.executablePath": "/usr/local/bin/fatou"
}
```

The extension's
[README](https://github.com/jolars/fatou/blob/main/editors/code/README.md)
documents the available settings and their defaults.

### Shared user config

To use a shared `fatou.toml` outside your projects, add this to your user
`settings.json`:

```json
{
  "fatou.serverEnv": {
    "FATOU_CONFIG": "/path/to/fatou.toml"
  }
}
```

Run **Fatou: Restart Server** after changing this setting or editing the shared
file. Project configs take precedence, and the files are never merged. Use an
absolute path; an unset or empty value uses the normal user config location. See
the [configuration guide](configuration.md#resolution-order) for the full
resolution order and error behavior.

### Using only some features

You can choose which parts of Fatou VS Code uses:

- `fatou.formatting.enable` — formatting.
- `fatou.diagnostics.enable` — parse errors and lint diagnostics.
- `fatou.languageFeatures.enable` — hover, completion, navigation, symbols,
  rename, and code actions.

All three default to `true`, and changes take effect without restarting the
server. For a formatter-only setup, turn off the other two:

```json
{
  "fatou.diagnostics.enable": false,
  "fatou.languageFeatures.enable": false
}
```

Turning off diagnostics also hides parse errors. To keep parse errors and
disable only specific lint rules, configure [`[lint]` in
`fatou.toml`](configuration.md#choosing-lint-rules) instead. Those settings
apply to every editor and the CLI.

## VSCodium and other Code OSS editors

The same extension is published to the [Open VSX
Registry](https://open-vsx.org/extension/jolars/fatou), which VSCodium and most
Code OSS builds use by default:

```bash
codium --install-extension jolars.fatou
```

If your build ships a different registry, download the VSIX matching your OS and
architecture from the Open VSX page and install it with **Extensions: Install
from VSIX…**, or from the command line:

```bash
codium --install-extension fatou-linux-x64.vsix
```

Settings are identical to VS Code's.

## Cursor

Search for **Fatou** in the Extensions view and install it. If your Cursor build
does not list it, download the VSIX for your platform from [Open
VSX](https://open-vsx.org/extension/jolars/fatou) and install it with
**Extensions: Install from VSIX…** from the command palette.

Cursor reads the same `settings.json` keys as VS Code, so the format-on-save and
binary-selection snippets above apply unchanged.

## Zed

Install the [Julia extension](https://github.com/JuliaEditorSupport/zed-julia)
first, then install **Fatou** from the extensions view (`zed: extensions` in the
command palette).

Zed uses the `fatou` on your `PATH` when there is one, and otherwise downloads
the release binary matching your platform. On systems such as NixOS, use an
installed binary if the downloaded release cannot run.

To run Fatou as the only Julia language server and use it for formatting, add
this to `settings.json`:

```json
{
  "languages": {
    "Julia": {
      "language_servers": ["fatou-language-server"],
      "formatter": {
        "language_server": {
          "name": "fatou-language-server"
        }
      }
    }
  }
}
```

To format on save, also add `"format_on_save": "on"` under `languages.Julia`.

To pass settings through Zed, put them under the server's name:

```json
{
  "lsp": {
    "fatou-language-server": {
      "settings": {
        "format": { "line-width": 100 },
        "lint": { "ignore": ["unused-binding"] }
      }
    }
  }
}
```

Use [`fatou.toml`](#configuration) for settings the whole team should share. A
discovered config file takes precedence over these editor settings.

To point Zed at a particular binary, set `binary.path`:

```json
{
  "lsp": {
    "fatou-language-server": {
      "binary": { "path": "/opt/fatou/bin/fatou", "arguments": ["lsp"] }
    }
  }
}
```

Keep `"lsp"` in `arguments`; this setting replaces the extension's default
arguments.

## Neovim

### Neovim 0.11+ (built-in `vim.lsp.config`)

Add to your config (e.g. `init.lua` or a file under `lua/`):

```lua
vim.lsp.config("fatou", {
  cmd = { "fatou", "lsp" },              -- or the absolute path to the binary
  filetypes = { "julia" },
  root_markers = { "Project.toml", "JuliaProject.toml", ".git" },
})
vim.lsp.enable("fatou")
```

Format on save:

```lua
vim.api.nvim_create_autocmd("BufWritePre", {
  pattern = "*.jl",
  callback = function() vim.lsp.buf.format({ name = "fatou" }) end,
})
```

### Older Neovim (autocmd + `vim.lsp.start`)

```lua
vim.api.nvim_create_autocmd("FileType", {
  pattern = "julia",
  callback = function(args)
    vim.lsp.start({
      name = "fatou",
      cmd = { "fatou", "lsp" },
      root_dir = vim.fs.root(args.buf, { "Project.toml", "JuliaProject.toml", ".git" }),
    })
  end,
})
```

## ALE (Vim and Neovim)

[ALE](https://github.com/dense-analysis/ale) includes a Fatou linter and fixer
for Julia. Install ALE and make sure `fatou` is on your `PATH`, then add to your
`.vimrc` or `init.vim`:

```vim
let g:ale_linters = {'julia': ['fatou']}
let g:ale_fixers = {'julia': ['fatou']}
```

If you already configure these dictionaries for other languages, add the `julia`
entries to them.

The linter starts `fatou lsp` to provide diagnostics and language features. The
fixer runs `fatou format`; use `:ALEFix` to format the current buffer. To run
configured fixers automatically on save, add:

```vim
let g:ale_fix_on_save = 1
```

To select a particular Fatou binary, set:

```vim
let g:ale_julia_fatou_executable = '/opt/fatou/bin/fatou'
```

This applies to both the linter and fixer. Configure project behavior through
[`fatou.toml`](configuration.md). See `:help ale-julia-fatou` for ALE's
Fatou-specific options.

## Helix

Add to `~/.config/helix/languages.toml`:

```toml
[language-server.fatou]
command = "fatou"
args = ["lsp"]

[[language]]
name = "julia"
language-servers = ["fatou"]
auto-format = true
```

Listing `language-servers` replaces Helix's default for Julia, which is
LanguageServer.jl. To use both servers while Fatou handles formatting, list them
together and disable formatting from the other server:

```toml
[[language]]
name = "julia"
language-servers = [{ name = "julia", except-features = ["format"] }, "fatou"]
auto-format = true
```

`hx --health julia` shows which servers Helix resolved for the language.

## Other LSP clients

For a client without a Fatou integration, configure it to:

1. Start `fatou lsp` over stdio for Julia (`*.jl`) files.
2. Find the project root using `Project.toml`, `JuliaProject.toml`, or `.git`.

Fatou discovers its configuration from the source file's directory, so no
additional settings are required.

Also attach the server to `Project.toml` and `Manifest.toml` if you want live
diagnostics and [dependency navigation and
updates](language-server.md#manage-your-package-environment) in those files. The
VS Code extension does this automatically.

## Configuration

Put shared formatting and lint settings in a `fatou.toml` at your project root.
The editor and CLI use the same file. See [Configuration](configuration.md) for
available settings and where Fatou looks for them.

To keep settings in your editor instead, use the same schema as `fatou.toml`.
For example, in Helix:

```toml
[language-server.fatou.config.format]
line-width = 100

[language-server.fatou.config.lint]
ignore = ["unused-binding"]
```

A discovered config file takes precedence over editor settings and replaces them
entirely. This also applies to a [global or `FATOU_CONFIG`
file](configuration.md#resolution-order).

For other LSP clients, send settings through `initializationOptions` or
`workspace/didChangeConfiguration`, either directly or wrapped in a `"fatou"`
key.

## Check it works

Open a `.jl` file containing `x=1` and format the buffer. It should become
`x = 1`.

  | Editor         | Format the buffer           | Apply a diagnostic's quick fix         |
  | -------------- | --------------------------- | -------------------------------------- |
  | VS Code family | **Format Document**         | Click the lightbulb beside the finding |
  | Neovim         | `:lua vim.lsp.buf.format()` | `:lua vim.lsp.buf.code_action()`       |
  | Helix          | `:format`                   | `<space>a`                             |

Diagnostics appear as you edit. When a finding has a suggested fix, open the
quick-fix menu to apply it.

<span id="notes"></span>

## Troubleshooting

If Fatou does not start, check that your editor can find the binary. An editor
launched from a desktop shortcut may have a different `PATH` from your terminal.
Try setting the binary's absolute path using the instructions for your editor
above.

If another server also formats Julia files, select Fatou explicitly. In Neovim,
use `vim.lsp.buf.format({ name = "fatou" })`. In Helix, exclude `format` from
the other server as shown above. In VS Code, set `editor.defaultFormatter` for
`[julia]` to `"jolars.fatou"`.
