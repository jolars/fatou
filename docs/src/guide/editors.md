# Editor Setup

Fatou includes a language server (`fatou lsp`, stdio JSON-RPC) that any LSP
client can drive. It provides formatting (whole document and range), lint and
parse diagnostics with quick fixes, completion, hover, signature help, go-to
definition, references, rename (of symbols, and of files and folders — moving a
file rewrites the `include` paths that name it), document and workspace symbols,
call and type hierarchy, folding ranges, document links, selection ranges, and
semantic tokens.

## Prerequisites

Except in the VS Code family, where the extension bundles a binary, install
Fatou (see [Getting Started](getting-started.md)) and make sure the `fatou`
binary is on your `PATH`, or note its absolute path.

## VS Code

Install the [Fatou
extension](https://marketplace.visualstudio.com/items?itemName=jolars.fatou)
(`jolars.fatou`) from the Marketplace, or from the command line:

```bash
code --install-extension jolars.fatou
```

The extension activates on Julia files, starts `fatou lsp` for you, and
registers itself as the default formatter for `[julia]`. Each platform-specific
build bundles a matching `fatou` binary, so nothing else is needed; on a
platform without one, it downloads a binary from GitHub releases.

To format on save, add to `settings.json`:

```json
{
  "[julia]": {
    "editor.defaultFormatter": "jolars.fatou",
    "editor.formatOnSave": true
  }
}
```

To use a `fatou` you installed yourself instead of the bundled one:

```json
{
  "fatou.executableStrategy": "environment"
}
```

or point at an exact binary:

```json
{
  "fatou.executableStrategy": "path",
  "fatou.executablePath": "/usr/local/bin/fatou"
}
```

The extension's
[README](https://github.com/jolars/fatou/blob/main/editors/code/README.md)
documents the available settings and their defaults.

### Using Only Some Features

The formatter, linter, and language features share one server but can be turned
off independently, so you can adopt just the parts you want:

- `fatou.formatting.enable` — use Fatou as a formatter.
- `fatou.diagnostics.enable` — show Fatou diagnostics (the linter).
- `fatou.languageFeatures.enable` — hover, completion, navigation, symbols,
  rename, code actions, and the rest.

All three default to `true`. They are client-side gates, so the server keeps
running and the toggles take effect without a restart. For a formatter-only
setup, turn off the other two:

```json
{
  "fatou.diagnostics.enable": false,
  "fatou.languageFeatures.enable": false
}
```

Turning off `fatou.diagnostics.enable` suppresses **every** diagnostic,
including the parse errors that a `fatou.toml` [`[lint]`
selection](../reference/configuration.md#lint) cannot silence. The `fatou.toml`
route stays the right tool when you want to keep parse errors but mute specific
lint rules across every editor and the CLI.

## VSCodium and Other Code OSS Editors

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
LanguageServer.jl. To keep it for the features Fatou does not cover yet while
Fatou handles formatting, list both and take formatting away from the other
server:

```toml
[[language]]
name = "julia"
language-servers = [{ name = "julia", except-features = ["format"] }, "fatou"]
auto-format = true
```

`hx --health julia` shows which servers Helix resolved for the language.

## Other LSP clients

Any client that speaks LSP over stdio works: launch `fatou lsp` with no
arguments for `*.jl` files, rooted at `Project.toml`, `JuliaProject.toml`, or
`.git`. Nothing else is required, because Fatou discovers its own configuration
from the file's directory upward.

Fatou also checks your `Project.toml` and `Manifest.toml` themselves, and
publishes those findings on the file at fault whether or not it is open. If you
additionally attach the server to those files (the VS Code extension does), an
open one reports its TOML errors as you type, before you save; language
features such as hover and formatting stay off for them, since they are not
Julia.

## Configuration

Fatou reads its settings from a `fatou.toml` next to your project (see the
[Configuration guide](configuration.md)), which is the recommended way to
configure it in any editor, since the whole team gets the same behavior.

A client can also push settings over LSP, as `initializationOptions` or
`workspace/didChangeConfiguration`, using the same schema as the file, either
bare or wrapped in a `"fatou"` key the way VS Code namespaces settings. In
Helix, for example:

```toml
[language-server.fatou.config.format]
line-width = 100

[language-server.fatou.config.lint]
ignore = ["unused-binding"]
```

A discovered `fatou.toml` shadows editor-pushed settings entirely rather than
merging with them, so a project file always wins.

## Unicode Symbol Input

Completion also offers the LaTeX and emoji sequences the Julia REPL substitutes
on tab, so `\alpha` inserts `α`, `\_1` inserts `₁`, and `\:smile:` inserts 😄.
Type the backslash to open the list, keep typing to narrow it, and accepting an
entry replaces the whole sequence, backslash included. As in the REPL, a bare
`\` lists the LaTeX sequences and `\:` opens the emoji. The table comes from the
running Julia's `REPL.REPLCompletions`, so it matches what your REPL does.

Two places stay quiet, so the list does not get in the way:

- inside a string macro or command literal (`r"\d"`, `raw"\n"`, `` `ls \d` ``),
  where every backslash belongs to the literal itself;
- on a lone escape in a plain string, so typing `"\n` does not offer `\nabla`.
  A second character brings the sequences back, so `\nu` and `\alpha` still
  work in strings and docstrings.

## Check It Works

Open a `.jl` file containing `x=1` and format the buffer: it becomes `x = 1`. In
Neovim that is `:lua vim.lsp.buf.format()`, in Helix `:format`, and in the VS
Code family **Format Document**. Diagnostics appear inline, and a lint finding
with a fix offers it as a quick fix (`:lua vim.lsp.buf.code_action()`,
`<space>a` in Helix, or the lightbulb in VS Code).

## Notes

- Document sync is incremental, and both whole-document and range formatting are
  supported.
- Multiple formatters attached? In Neovim, pass `{ name = "fatou" }` to
  `vim.lsp.buf.format()`; in Helix, strip `format` from the other server as
  shown above; in VS Code, set `editor.defaultFormatter` for `[julia]`.
- `undefined-name` and `call-arity` need project context, so the language server
  enables them for workspace member files even though the command line leaves
  them opt-in. An `ignore` entry still turns them off.
