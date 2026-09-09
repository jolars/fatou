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

## pre-commit

Run Fatou on staged Julia files with
[`fatou-pre-commit`](https://github.com/jolars/fatou-pre-commit). The hooks
install the Fatou binary from PyPI in an environment managed by pre-commit, so
you do not need to install Fatou, Rust, or Julia separately.

[Install pre-commit](https://pre-commit.com/#installation), then add this entry
to your `.pre-commit-config.yaml`:

```yaml
repos:
  - repo: https://github.com/jolars/fatou-pre-commit
    rev: v0.18.0
    hooks:
      - id: fatou-lint
      - id: fatou-format
```

Install the Git hook and run it once over all tracked files:

```bash
pre-commit install
pre-commit run --all-files
```

On subsequent commits, the hooks run on staged `.jl` files. `fatou-lint` reports
lint findings, and `fatou-format` formats files in place. When a hook changes a
file, review and stage the changes, then commit again.

To apply safe lint fixes, add `--fix` to the lint hook. Keep it before the
formatter so formatting runs after the rewrites:

```yaml
hooks:
  - id: fatou-lint
    args: [--fix]
  - id: fatou-format
```

To check formatting without changing files, add `args: [--check]` to
`fatou-format`.

Both hooks pass `--force-exclude`, so `exclude` and `extend-exclude` in your
`fatou.toml` apply even though pre-commit supplies filenames explicitly. See
[excluding files](configuration.md#excluding-files) for details.

The `rev` selects the Fatou version: `v0.18.0` installs Fatou 0.18.0. Run
`pre-commit autoupdate` to update configured hook revisions, then review and
commit the changes to `.pre-commit-config.yaml`.

## GitHub Actions

Use [`fatou-action`](https://github.com/jolars/fatou-action) to check formatting
and lint Julia code in CI. It installs and caches a prebuilt Fatou binary on
GitHub-hosted Linux, macOS, and Windows runners.

Create `.github/workflows/fatou.yml`:

```yaml
name: Fatou

on:
  pull_request:
  push:
    branches: [main]

permissions:
  contents: read

jobs:
  fatou:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: jolars/fatou-action@v1
        with:
          version: v0.18.0
```

By default, the action runs `fatou format --check` and `fatou lint` from the
repository root. Formatting differences or lint findings fail the check. It uses
the same [configuration
discovery](configuration.md#where-fatou-looks-for-a-config) as the CLI.

The action's `@v1` tag selects the action version; the `version` input selects
the Fatou CLI version. Keep the latter aligned with your pre-commit revision and
local installation for consistent results. Omitting `version` selects the latest
Fatou release with a binary for the runner.

Set inputs under `with` to customize the check:

  | Input    | Purpose                                               | Default             |
  | -------- | ----------------------------------------------------- | ------------------- |
  | `path`   | File or directory to check                            | `.`                 |
  | `format` | Run the formatting check                              | `"true"`            |
  | `lint`   | Run the lint check                                    | `"true"`            |
  | `config` | Load an explicit `fatou.toml` path                    | Automatic discovery |
  | `quiet`  | List files needing formatting without printing a diff | `"false"`           |

For example, to check only formatting under `src/`:

```yaml
- uses: jolars/fatou-action@v1
  with:
    version: v0.18.0
    path: src/
    lint: "false"
```

Set `format: "false"` to run only linting. See the [action
reference](https://github.com/jolars/fatou-action#inputs) for all inputs and
outputs.
