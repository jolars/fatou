# Integrations

## dprint

Format Julia files alongside other languages with [dprint](https://dprint.dev)
and [`dprint-plugin-fatou`](https://github.com/jolars/dprint-plugin-fatou). The
plugin bundles Fatou's formatter as WebAssembly, so you can format `.jl` files
without installing Julia or the Fatou CLI.

### Setup

[Install dprint](https://dprint.dev/install/). If your project does not yet have
a `dprint.json`, create one with `dprint init`. Then add Fatou:

```bash
dprint config add jolars/fatou
```

The command adds a versioned plugin URL to the `plugins` array in your dprint
configuration. Commit that configuration so contributors and CI use the same
plugin version.

Format the project's files in place:

```bash
dprint fmt
```

For CI, check formatting without writing changes:

```bash
dprint check
```

The check exits nonzero when files need formatting. Both commands run all
configured plugins; Fatou handles `.jl` files. See the [dprint CLI
reference](https://dprint.dev/cli/) for file selection and other options.

### Configuration

Add a `fatou` object to your `dprint.json`, keeping the `plugins` array created
above. For example, merge in these settings:

```json
{
  "fatou": {
    "lineWidth": 92,
    "indentWidth": 4,
    "lineEnding": "auto"
  }
}
```

Every setting is optional. Values in `fatou` override dprint's global settings:

  | Key           | Values                         | Default when omitted                   |
  | ------------- | ------------------------------ | -------------------------------------- |
  | `lineWidth`   | Nonnegative integer            | Global `lineWidth`, otherwise `92`     |
  | `indentWidth` | Nonnegative integer            | Global `indentWidth`, otherwise `4`    |
  | `lineEnding`  | `auto`, `lf`, `crlf`, `native` | Global `newLineKind`, otherwise `auto` |

`lineWidth` is the target maximum line width, and `indentWidth` is the number of
spaces per indentation level. Fatou always uses spaces; dprint's global
`useTabs` setting has no effect.

`lineEnding: "auto"` follows each file's first line ending, falling back to LF
when the file has none. Use `lf` or `crlf` to force a particular style. The
plugin also accepts `native`, which resolves to LF in its WebAssembly build,
including when dprint runs on Windows.

The plugin reads its settings from dprint's configuration and does not load
`fatou.toml`. If you also use the Fatou CLI or language server, keep the
corresponding `[format]` settings in sync: `line-width`, `indent-width`, and
`line-ending`. See [Fatou's formatting
configuration](configuration.md#formatting).

dprint controls file discovery through its top-level `includes` and `excludes`
settings and `.gitignore`. Put exclusions for this workflow in `dprint.json`;
the plugin does not apply exclusions from `fatou.toml`. See [dprint's
configuration guide](https://dprint.dev/config/).

### Updates

The plugin is [released
independently](https://github.com/jolars/dprint-plugin-fatou/releases) of the
Fatou CLI. To update the plugins listed in your dprint configuration, run:

```bash
dprint config update
```

Review and commit the configuration changes. Updating the Fatou CLI does not
update the formatter bundled in the plugin. To get matching output from both,
use releases with the same `fatou-formatter` version and equivalent settings.

The plugin provides formatting. For linting and the full language server, use
the [Fatou CLI](getting-started.md) and [editor integration](editors.md).
