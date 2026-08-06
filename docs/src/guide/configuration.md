# Configuration

Fatou is configured with a TOML file named `fatou.toml`. Every key is optional,
so a config only needs to mention what you want to change from the defaults.
Unknown keys are rejected with an error, which means a typo never silently falls
back to a default.

A minimal project config looks like this:

```toml
exclude = ["vendored/"]

[format]
line-width = 100

[lint]
ignore = ["unused-argument"]
```

This guide covers the common tasks. For the exhaustive list of keys, their
types, and their defaults, see the [configuration
reference](../reference/configuration.md).

## Where Fatou looks for a config

For a given file, Fatou walks up from the file's directory through its
ancestors, and uses the first `fatou.toml` it finds. The usual layout is a
single `fatou.toml` beside `.git` at the root of the project.

The walk stops at the repository root, so a `fatou.toml` parked above your
repository never governs the project inside it. The root itself is searched, and
a worktree or submodule checkout, whose `.git` is a file rather than a
directory, bounds the walk the same way. A directory with no `.git` ancestor
keeps walking to the filesystem root.

### User-wide defaults

If you want the same settings across projects, do not put a `fatou.toml` above
your repositories; use a global config instead. When no project `fatou.toml` is
found, Fatou looks for one in your user config directory, typically
`~/.config/fatou/fatou.toml`.

To keep a config on a synced drive and point every machine at it, set the
`FATOU_CONFIG` environment variable to its path. A set `FATOU_CONFIG` shadows
the global config entirely, and a missing or malformed file there is a hard
error rather than a silent fall-through, so a typo'd path cannot go unnoticed.

Both are whole-file fallbacks, never merged with a project config: as soon as a
project `fatou.toml` is found, it is the only file that applies. Relative
`exclude` patterns in a `FATOU_CONFIG` or global file resolve against the
working directory (on the command line) or the document's directory (in the
language server) rather than the config file's own directory.

The language server uses the same resolution, so either file is a convenient way
to set editor-wide defaults. Only project files are watched, so an edit to a
global or `FATOU_CONFIG` file is picked up when the server restarts.

### Bypassing discovery

On the command line, `--config <PATH>` loads an explicit file and skips
discovery altogether, and `--no-config` ignores every file (project,
`FATOU_CONFIG`, and global) and runs with the built-in defaults.

### Resolution order

In full, Fatou uses the first source that applies:

1. `--config <PATH>`, which loads that file and skips discovery.
2. `--no-config`, which ignores every file and uses the built-in defaults.
3. The nearest `fatou.toml`, found by walking up from the file's directory. The
   walk stops at the repository root (the directory holding `.git`, whether a
   directory or a file), inclusive; a directory with no `.git` ancestor is
   walked to the filesystem root.
4. `$FATOU_CONFIG`, when set. A missing or malformed file here is an error.
5. The global user config: the first existing file among
   1. `$XDG_CONFIG_HOME/fatou/fatou.toml`, when that variable is set
   2. `~/.config/fatou/fatou.toml`
   3. the platform config directory, on macOS
      `~/Library/Application Support/fatou/fatou.toml`
6. The built-in defaults.

Sources are never merged: exactly one file is used.

## Excluding files

`exclude` takes gitignore-style patterns, resolved relative to the directory
containing `fatou.toml`. Excluded directories are pruned during discovery, so
`fatou format src` and `fatou lint src` never descend into them.

```toml
exclude = ["vendored/"]
```

Use `extend-exclude` when you want to keep whatever `exclude` already lists and
add to it, which is mostly useful when the two live in different layers of your
setup:

```toml
extend-exclude = ["generated.jl"]
```

A file named explicitly on the command line is always processed, even if it
matches an exclude pattern. Pass `--force-exclude` to apply the patterns to
explicitly named files too; this is meant for runners like pre-commit, which
invoke Fatou with the staged files as arguments. Extra patterns can also be
supplied per run with `--exclude` on `fatou format` and `fatou lint`.

## Formatting

The `[format]` table controls the formatter. The defaults follow common Julia
conventions, so most projects only set a key here to depart from them:

```toml
[format]
line-width = 92
indent-width = 4
line-ending = "auto"
```

`line-width` is the width the formatter tries to keep lines within, and
`indent-width` is the number of spaces per indentation level. Both can be
overridden per run with the `--line-width` and `--indent-width` flags on `fatou
format`.

`line-ending` decides the newline style. The default, `auto`, mirrors the source
file's first line ending and falls back to `lf` when the file has none, which
keeps mixed checkouts stable. Use `lf` or `crlf` to force one style, or `native`
to follow the platform Fatou runs on.

> **Deprecation**: the snake_case keys `line_width` and `indent_width` are still
> accepted but print a warning. Use the kebab-case `line-width` and
> `indent-width` instead; the snake_case forms will be removed in a future
> release.

## Choosing lint rules

By default every rule runs. `ignore` turns individual rules off, and `select`,
when set, restricts the run to exactly the rules you list:

```toml
[lint]
select = ["unused-binding", "undefined-name"]
ignore = ["unused-argument"]
```

See the [rule reference](../reference/rules.md) for the available rule IDs.

`[lint.severity]` changes how loudly a rule reports, without changing whether it
runs. Rules you do not list keep their default severity:

```toml
[lint.severity]
unused-binding = "warning"
undefined-name = "error"
```

## Tuning a rule

A rule with a tunable knob reads it from its own table, named after the rule ID.
For example, [`discouraged-function`](../reference/rules/discouraged-function.md)
ships a deny-list of Base functions with process-wide or memory-unsafe effects,
and you can add your own entries to it:

```toml
[lint.rules.discouraged-function]
extend-functions = { sleep = "use a timer instead of blocking the task" }
```

Rules without options have no table. The [configuration
reference](../reference/configuration.md#lintrulesid) lists the rules that do.

Note that strictness here is deliberately different from `select`, `ignore`, and
`severity`. Those are lists of IDs you typed, so an unrecognized entry is only a
warning and the run continues. A `[lint.rules.<id>]` table is a *schema*, so a
misspelled rule ID, or a misspelled key inside one, is a configuration parse
error and the run stops.
