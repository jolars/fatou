# Configuration reference

This page lists the keys accepted in `fatou.toml`. All keys are optional.
Omitted keys use their defaults; unknown keys are rejected with an error.

`FATOU_CONFIG` and global config files use this same schema. For a task-oriented
walkthrough, including [configuration
resolution](../guide/configuration.md#resolution-order) and [user-wide
defaults](../guide/configuration.md#user-wide-defaults), see the [configuration
guide](../guide/configuration.md). A machine-readable [JSON
Schema](https://fatou.dev/fatou.schema.json) is also available for editor
completion and validation.

## Extending another config

`extend` resolves relative to the file that declares it. Absolute paths and `~/`
paths also work. Missing files and inheritance cycles are configuration errors.
See the [guide](../guide/configuration.md#extending-another-config) for a
shared-config recipe.

Sections merge key by key. Values in the extending file win, including lists
such as `exclude`, `select`, and `ignore`. `extend-exclude` and `extend-select`
lists accumulate across the chain, with inherited entries first.

Inherited `[project] entry-points` remain relative to the file that declares
them. Exclude patterns in an inherited file apply relative to the extending
config's directory, or to the working or document directory for an environment
or global config. See [top-level keys](#top-level-keys) for exclusion behavior.

## Top-level keys

  | Key              | Type             | Default | Description                                 |
  | ---------------- | ---------------- | ------- | ------------------------------------------- |
  | `extend`         | string           | unset   | Path to another config file to inherit.     |
  | `exclude`        | array of strings | `[]`    | Patterns to exclude from file discovery.    |
  | `extend-exclude` | array of strings | `[]`    | Additional patterns, appended to `exclude`. |

`exclude` and `extend-exclude` take gitignore-style patterns. `extend-exclude`
adds to the `exclude` list. Patterns resolve relative to the directory
containing the selected project or explicit config. For a `FATOU_CONFIG` or
global config, they resolve relative to the working directory on the command
line or the document's directory in the language server, rather than the config
file's own directory.

See [Extending another config](#extending-another-config) for path resolution
and merge behavior for `extend`.

Files named explicitly on the command line are processed even when they match a
pattern, unless `--force-exclude` is passed. Extra patterns can be added per run
with `--exclude` on `fatou format` and `fatou lint`.

```toml
exclude = ["vendored/"]
extend-exclude = ["generated.jl"]
```

## `[project]`

  | Key                   | Type             | Default                  | Description                      |
  | --------------------- | ---------------- | ------------------------ | -------------------------------- |
  | `entry-points`        | array of strings | Conventional files below | Replace the script entry points. |
  | `extend-entry-points` | array of strings | `[]`                     | Add script entry points.         |

The default `entry-points` list contains `test/runtests.jl`, `docs/make.jl`, and
`deps/build.jl`. Fatou discovers these files relative to Julia projects above
the lint target or open document, stopping at the repository boundary. Missing
default files are ignored. Setting `entry-points` replaces the default list;
`entry-points = []` disables it. `extend-entry-points` adds paths to either the
defaults or an explicit replacement list.

Configured paths can be literal files or globs, relative to the containing
configuration file. Globs such as `scripts/**/*.jl` expand to Julia files, honor
`.gitignore`, and contribute no files when unmatched. Invalid globs and missing
explicit literal paths are errors. Paths remain relative to the configuration
file when using `--config`, `FATOU_CONFIG`, or a global config. Duplicate paths
are ignored. Entry points must be configured in a file, rather than through
editor settings.

```toml
[project]
extend-entry-points = ["scripts/**/*.jl"]
```

Entry points let Fatou resolve names across static `include("path")` calls. The
[`undefined-name`](rules.md#undefined-name) rule is enabled by default for entry
points and their included files. An explicit `[lint] select` replaces these
defaults; `ignore = ["undefined-name"]` disables the rule.

Configuring entry points does not add files to the lint targets. Included files
can contribute definitions even when excluded from diagnostic reporting. Dynamic
includes or dependencies that cannot be analyzed can prevent undefined-name
checking. See the [configuration
guide](../guide/configuration.md#checking-standalone-scripts) for examples and
limitations.

## `[julia]`

  | Key       | Type   | Default | Description                                                       |
  | --------- | ------ | ------- | ----------------------------------------------------------------- |
  | `version` | string | unset   | Target Julia version or support range for compatibility checking. |

`version` supplies the support range used by
[`julia-version-compat`](rules.md#julia-version-compat). It accepts Julia Pkg
compatibility expressions: `"1.6"` means at least Julia 1.6.0 and below 2.0.0,
`"1.6 - 1.11"` is an inclusive range, and commas form unions. Compatibility
checks use the lowest supported version in the range.

When unset, Fatou discovers the target from `Project.toml`'s `[compat].julia`,
then `Manifest.toml`'s `julia_version`. The `--julia-version` flag on
`fatou lint` overrides this setting. An invalid expression produces a warning
and leaves the setting unset.

```toml
[julia]
version = "1.6"
```

## `[format]`

  | Key            | Type    | Default  | Description                                                |
  | -------------- | ------- | -------- | ---------------------------------------------------------- |
  | `line-width`   | integer | `92`     | The width the formatter tries to keep lines within.        |
  | `indent-width` | integer | `4`      | Number of spaces per indentation level.                    |
  | `line-ending`  | string  | `"auto"` | The newline style emitted at the end of each line.         |
  | `safe`         | boolean | `false`  | Verify that formatting preserves the program and comments. |

`line-width` and `indent-width` can be overridden per run with the
`--line-width` and `--indent-width` flags on `fatou format`.

`safe = true` verifies that formatting preserves program structure and comments
in both the CLI and language server. Malformed or unverifiable input, changed
program structure, or changed comments cause an error without writing files or
returning formatting edits. Range formatting verifies the entire document,
including code outside the selection. `fatou format --safe` enables verification
even when the config sets `safe = false`.

`line-ending` accepts:

- `auto` (default): mirror the source file's first line ending, defaulting to
  `lf` when the file has none.
- `lf`: always `\n` (Unix).
- `crlf`: always `\r\n` (Windows).
- `native`: `\n` on Unix, `\r\n` on Windows.

```toml
[format]
line-width = 92
indent-width = 4
line-ending = "auto"
```

> **Deprecation**: the snake_case keys `line_width` and `indent_width` are still
> accepted but print a warning. Use the kebab-case `line-width` and
> `indent-width` instead.

## `[lint]`

  | Key             | Type             | Default | Description                                                   |
  | --------------- | ---------------- | ------- | ------------------------------------------------------------- |
  | `select`        | array of strings | unset   | Base rule IDs to enable, replacing the defaults when set.     |
  | `extend-select` | array of strings | `[]`    | Additional rule IDs to enable alongside `select` or defaults. |
  | `ignore`        | array of strings | `[]`    | Rule IDs to disable, including those in `extend-select`.      |
  | `severity`      | table            | `{}`    | Per-rule severity overrides.                                  |
  | `rules`         | table            | `{}`    | Per-rule option tables.                                       |

By default most rules run; the [rule reference](rules.md) lists the opt-in rules
and all available rule IDs. An unrecognized ID in `select`, `extend-select`,
`ignore`, or `severity` is a warning, not an error.

Fatou starts with the defaults, or `select` when set, adds `extend-select`, and
then removes rules in `ignore`. Repeated IDs do not run a rule more than once.
An empty `select = []` disables the defaults; `extend-select` can still add
rules.

Default rule selection depends on the file context:

- Workspace package files also enable rules that need project resolution.
- Declared script entry points and their static includes also enable
  `undefined-name`.
- Loose files use the conservative defaults listed in the rule reference.

An explicit `select` replaces all of these context-dependent defaults.

`[lint.severity]` maps a rule ID to the severity its findings report, one of
`"error"`, `"warning"`, `"info"`, or `"hint"`. Rules not listed keep their
default severity.

```toml
[lint]
extend-select = ["undefined-name"]
ignore = ["unused-binding"]

[lint.severity]
undefined-name = "error"
```

## `[lint.rules.<id>]`

Configure rule options in a table named after the rule ID. Only rules with
options accept a table. Unknown rule IDs or option keys in these tables are
configuration errors. Set severity separately in [`[lint.severity]`](#lint).

### `[lint.rules.discouraged-function]`

Options for [`discouraged-function`](rules.md#discouraged-function). Both keys
are tables mapping a function name to the suggestion shown in the diagnostic.

  | Key                | Type  | Default          | Description                                                                    |
  | ------------------ | ----- | ---------------- | ------------------------------------------------------------------------------ |
  | `functions`        | table | the built-in set | Replaces the built-in deny-list.                                               |
  | `extend-functions` | table | `{}`             | Adds to `functions`; an entry here also wins over a built-in of the same name. |

The built-in set covers Base functions with process-wide or memory-unsafe
effects: `exit`, `cd`, `redirect_stdout`, `redirect_stderr`, `unsafe_load`,
`unsafe_store!`, `unsafe_wrap`, `unsafe_string`, `pointer_from_objref`, and
`unsafe_pointer_to_objref`.

Setting `functions = {}` removes the built-in list; only entries in
`extend-functions`, if any, are then checked.

```toml
# Add to the built-in list.
[lint.rules.discouraged-function]
extend-functions = { sleep = "use a timer instead of blocking the task" }

# Or replace the built-in list.
# functions = { my_legacy_helper = "call `new_helper` instead" }
```
