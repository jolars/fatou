# Fatou

A language server, formatter, and linter for Julia.

## Quick start

1. Install the **Fatou** extension.
2. Open a Julia file (`.jl`).
3. The extension starts `fatou lsp` automatically.

By default, the extension uses a `fatou` binary that ships inside the extension
(one platform-specific VSIX per OS/architecture), falling back to downloading a
matching binary from GitHub releases when none is bundled.

## Features

- Starts `fatou lsp` automatically when you open Julia files.
- Formats Julia code using Fatou's deterministic formatter.
- Surfaces Fatou diagnostics in the editor.
- Registers itself as the default formatter for `[julia]` files.

## Commands

- `Fatou: Restart Server`: stops and restarts the Fatou language server
  (re-reads settings and re-resolves the binary). Useful if the LSP gets wedged
  or after changing settings such as `fatou.version` or `fatou.executablePath`.

## Binary installation

By default, the extension uses a `fatou` binary that ships inside the extension
itself (one platform-specific VSIX per OS/architecture). No download, no GitHub
round-trip, and the language server starts on first activation even on
restricted or offline networks. Behavior is controlled by
`fatou.executableStrategy`:

- `bundled` (default): use the binary that ships inside the extension. If you're
  on a platform without a platform-specific build (or you've installed the
  universal VSIX), the extension falls back to downloading a matching binary
  from GitHub releases.
- `environment`: look for `fatou` on the system `PATH`.
- `path`: use the binary at `fatou.executablePath`.

If you set `fatou.version` or `fatou.releaseTag` explicitly, the bundled binary
is skipped and the requested version is downloaded from GitHub. When
`fatou.version` is `latest`, the extension selects the most recent stable
release that contains a matching platform asset.

## Common setup examples

Use a local binary at a fixed path:

```json
{
  "fatou.executableStrategy": "path",
  "fatou.executablePath": "/usr/local/bin/fatou"
}
```

Use whatever `fatou` is on your `PATH`:

```json
{
  "fatou.executableStrategy": "environment"
}
```

Pin to a specific release:

```json
{
  "fatou.version": "0.2.0",
  "fatou.githubRepo": "jolars/fatou"
}
```

Use `fatou.releaseTag` only if you need an exact tag override:

```json
{
  "fatou.releaseTag": "v0.2.0"
}
```

## Requirements and troubleshooting

- **NixOS**: the bundled binary won't run because of the dynamic loader path.
  The extension detects NixOS and skips the download, using `fatou` on your
  `PATH` instead; install `fatou` and set `fatou.executableStrategy` to
  `environment`, or use `path` with `fatou.executablePath`.
- **Offline/restricted networks/proxies**: the bundled-binary default works
  without network access. Only the explicit-version download paths
  (`fatou.version`/`fatou.releaseTag`) require GitHub connectivity.
- If a download fall-through fails, the extension shows a warning and falls back
  to looking up `fatou` on the system `PATH`.

## Settings

Fatou registers itself as the default formatter for `[julia]` files.

- `fatou.executableStrategy`: how to locate the `fatou` binary: `bundled`
  (default), `environment`, or `path`.
- `fatou.executablePath`: path to the binary, used only when
  `executableStrategy` is `path`.
- `fatou.version`: version to install (default: `"latest"`).
- `fatou.releaseTag`: advanced exact tag override (takes precedence if
  explicitly set).
- `fatou.githubRepo`: GitHub repo for downloads (default: `"jolars/fatou"`).
- `fatou.serverArgs`: extra args after `fatou lsp`.
- `fatou.serverEnv`: extra environment variables.
- `fatou.extraPath`: extra PATH entries prepended for the language server
  process.
- `fatou.logLevel`: log level for the language server, mapped to `RUST_LOG`
  (`off`, `error`, `warn`, `info`, `debug`, `trace`; unset by default).
  `fatou.serverEnv.RUST_LOG` overrides this if both are set.
- `fatou.trace.server`: LSP trace level (`off`, `messages`, `verbose`).

## Security and trust

When `fatou.executableStrategy` is `bundled` (the default), the extension
prefers the binary that shipped inside the VSIX. If no bundled binary is
available, or `fatou.version`/`fatou.releaseTag` is set explicitly, it downloads
from GitHub releases configured by `fatou.githubRepo` (default `jolars/fatou`).
