# Configuration Reference

Every key accepted in `fatou.toml`. All keys are optional. Omitting a key uses
its default. Unknown keys are rejected with an error.

`FATOU_CONFIG` and global config files use this same schema. For a task-oriented
walkthrough, see the [configuration guide](../guide/configuration.md).

## Top-level keys

  | Key              | Type             | Default | Description                                 |
  | ---------------- | ---------------- | ------- | ------------------------------------------- |
  | `exclude`        | array of strings | `[]`    | Patterns to exclude from file discovery.    |
  | `extend-exclude` | array of strings | `[]`    | Additional patterns, appended to `exclude`. |

Both keys take gitignore-style patterns, resolved relative to the directory
containing `fatou.toml` (or, for a `FATOU_CONFIG` or global config, the working
directory). Excluded directories are pruned during discovery.

Files named explicitly on the command line are processed even when they match a
pattern, unless `--force-exclude` is passed. Extra patterns can be added per run
with `--exclude` on `fatou format` and `fatou lint`.

```toml
exclude = ["vendored/"]
extend-exclude = ["generated.jl"]
```

## `[format]`

  | Key            | Type    | Default  | Description                                         |
  | -------------- | ------- | -------- | --------------------------------------------------- |
  | `line-width`   | integer | `92`     | The width the formatter tries to keep lines within. |
  | `indent-width` | integer | `4`      | Number of spaces per indentation level.             |
  | `line-ending`  | string  | `"auto"` | The newline style emitted at the end of each line.  |

`line-width` and `indent-width` can be overridden per run with the
`--line-width` and `--indent-width` flags on `fatou format`.

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
> `indent-width` instead; the snake_case forms will be removed in a future
> release.

## `[lint]`

  | Key        | Type             | Default | Description                      |
  | ---------- | ---------------- | ------- | -------------------------------- |
  | `select`   | array of strings | unset   | If set, only these rule IDs run. |
  | `ignore`   | array of strings | `[]`    | Rule IDs to disable.             |
  | `severity` | table            | `{}`    | Per-rule severity overrides.     |
  | `rules`    | table            | `{}`    | Per-rule option tables.          |

See the [rule reference](rules.md) for the available rule IDs. An unrecognized
ID in `select`, `ignore`, or `severity` is a warning, not an error.

`[lint.severity]` maps a rule ID to the severity its findings report, one of
`"error"`, `"warning"`, `"info"`, or `"hint"`. Rules not listed keep their
default severity.

```toml
[lint]
select = ["some-rule"]
ignore = ["another-rule"]

[lint.severity]
some-rule = "error"
```

## `[lint.rules.<id>]`

A rule with a tunable knob reads it from its own table, named after the rule ID.
Rules without options have no table. Keys are kebab-case, matching the rest of
the file.

Unlike `select`, `ignore`, and `severity`, these tables are a *schema*: a
misspelled rule ID, or a misspelled key inside one, is a configuration parse
error and the run stops.

Per-rule *severity* is not set here; use [`[lint.severity]`](#lint) for that.

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

Setting `functions = {}` silences the rule without having to `ignore` it, which
is the way to keep the rule available for a future project-specific list.

```toml
# Keep the built-ins and add a project rule of your own.
[lint.rules.discouraged-function]
extend-functions = { sleep = "use a timer instead of blocking the task" }

# Or replace the built-ins outright.
# functions = { my_legacy_helper = "call `new_helper` instead" }
```
