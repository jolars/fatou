# Editor Setup

Fatou ships a language server (`fatou lsp`, stdio JSON-RPC) that already
advertises **document formatting** and pushes **parse diagnostics**. This guide
wires it into your editor so you can format Julia buffers and see parse errors
inline.

> Coverage is growing construct by construct; constructs without a formatting
> rule yet are left byte-identical, so formatting is always safe to run.

## Prerequisites

Install Fatou (see [Getting Started](../getting-started.md)) and make sure the
`fatou` binary is on your `PATH`, or note its absolute path.

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

### Try it

Open a `.jl` file containing `x=1`, then run `:lua vim.lsp.buf.format()` (or
just save with the autocmd above). It becomes `x = 1`. Parse errors, if any,
appear as diagnostics (`:lua vim.diagnostic.open_float()`).

## Notes

- The server uses **full-document sync** and full-document formatting today;
  range formatting (`textDocument/rangeFormatting`) is on the roadmap.
- Multiple formatters? `vim.lsp.buf.format({ name = "fatou" })` forces Fatou
  even if another Julia LSP is attached.
