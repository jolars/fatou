# Configuration

Fatou is configured with a TOML file named `fatou.toml`. All keys are optional;
omitting a key uses its default. Unknown keys are rejected with an error, so a
typo never silently falls back to a default.

## Discovery

For a given file, Fatou looks for `fatou.toml` by walking up from the file's
directory through its ancestors, stopping at the first `fatou.toml` it finds.

The walk also stops at the repository root, so a `fatou.toml` above the
repository never governs the project inside it. The root itself is searched, so
the usual layout (`fatou.toml` beside `.git`) still applies, and a worktree or
submodule checkout, whose `.git` is a file, bounds the walk the same way. A
directory with no `.git` ancestor keeps walking to the filesystem root. For
user-wide defaults, use the global config below rather than a file parked above
your repositories.

If no project `fatou.toml` is found, Fatou consults the `FATOU_CONFIG`
environment variable, which names the file to use instead of the global user
config below. This is handy for keeping one config on a synced drive and
pointing every machine at it. A set `FATOU_CONFIG` shadows the global config
entirely, and a missing or malformed file is a hard error rather than a silent
fall-through, so a typo'd path cannot go unnoticed.

If `FATOU_CONFIG` is unset, Fatou falls back to a global user config: the first
existing file among

1. `$XDG_CONFIG_HOME/fatou/fatou.toml`, when that variable is set
2. `~/.config/fatou/fatou.toml`
3. the platform config directory, on macOS
   `~/Library/Application Support/fatou/fatou.toml`

The `FATOU_CONFIG` and global files use the same schema as a project
`fatou.toml` and are whole-file fallbacks, never merged with a project config.
Relative `exclude` patterns in them resolve against the working directory (CLI)
or the document's directory (language server) rather than the config's own
directory. The language server uses the same resolution, so both are easy ways
to set editor-wide defaults; an edit to either is picked up when the server
restarts, since only project files are watched. If none of these files is found,
the built-in defaults apply.

On the command line:

- `--config <PATH>` loads an explicit file and skips discovery.
- `--no-config` ignores any file (project, `FATOU_CONFIG`, or global) and uses
  the built-in defaults.

## Top-level keys

  | Key              | Type             | Default | Description                                    |
  | ---------------- | ---------------- | ------- | ---------------------------------------------- |
  | `exclude`        | array of strings | `[]`    | Patterns to exclude from file discovery.       |
  | `extend-exclude` | array of strings | `[]`    | Additional patterns, appended to `exclude`.    |

Both keys take gitignore-style patterns, resolved relative to the directory
containing `fatou.toml` (or, for a `FATOU_CONFIG` or global config, the working
directory). Excluded directories are pruned during discovery, so `fatou format
src` and `fatou lint src` never descend into them.

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
