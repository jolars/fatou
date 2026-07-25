# Configuration

Fatou is configured with a TOML file named `fatou.toml`. All keys are optional;
omitting a key uses its default. Unknown keys are rejected with an error, so a
typo never silently falls back to a default.

## Discovery

For a given file, Fatou looks for `fatou.toml` by walking up from the file's
directory through its ancestors, stopping at the first `fatou.toml` it finds.

On the command line:

- `--config <PATH>` loads an explicit file and skips discovery.
- `--no-config` ignores any discovered file and uses the built-in defaults.

## Top-level keys

  | Key              | Type             | Default | Description                                    |
  | ---------------- | ---------------- | ------- | ---------------------------------------------- |
  | `exclude`        | array of strings | `[]`    | Patterns to exclude from file discovery.       |
  | `extend-exclude` | array of strings | `[]`    | Additional patterns, appended to `exclude`.    |

Both keys take gitignore-style patterns, resolved relative to the directory
containing `fatou.toml`. Excluded directories are pruned during discovery, so
`fatou format src` and `fatou lint src` never descend into them.

```toml
exclude = ["vendored/"]
extend-exclude = ["generated.jl"]
```

A file named explicitly on the command line is always processed, even if it
matches an exclude pattern. Pass `--force-exclude` to apply the patterns to
explicitly named files too; this is meant for runners like pre-commit that
invoke Fatou with the staged files as arguments. Extra patterns can also be
supplied per run with `--exclude` on `fatou format` and `fatou lint`.

## `[format]`

  | Key            | Type    | Default  | Description                                         |
  | -------------- | ------- | -------- | --------------------------------------------------- |
  | `line-width`   | integer | `92`     | The width the formatter tries to keep lines within. |
  | `indent-width` | integer | `4`      | Number of spaces per indentation level.             |
  | `line-ending`  | string  | `"auto"` | The newline style emitted at the end of each line.  |

Defaults follow common Julia conventions. The width keys can be overridden per
run with the `--line-width`/`--indent-width` flags on `fatou format`.

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

See the [rule reference](rules.md) for the available rule IDs.

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
