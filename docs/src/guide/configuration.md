# Configuration

Fatou is configured with a TOML file named `fatou.toml`. Every key is optional,
so a config only needs to mention what you want to change from the defaults.
Unknown keys are rejected with an error, which means a typo never silently falls
back to a default.

The [dprint plugin](integrations.md#configuration) uses the `fatou` object in
`dprint.json` for its settings and does not read `fatou.toml`.

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

## Editor Support

Fatou publishes a [JSON Schema](https://json-schema.org/) for `fatou.toml`, so
editors with TOML support can offer key and value completion, inline
documentation, and validation while you edit the configuration.

Schema URL : <https://fatou.dev/fatou.schema.json>

The schema is generated from the same configuration types Fatou uses at runtime
and checked for drift in the test suite.

### VS Code and Even Better TOML

With the [Even Better
TOML](https://marketplace.visualstudio.com/items?itemName=tamasfe.even-better-toml)
extension installed, add this association to your user or workspace
`settings.json`:

```jsonc
{
  "evenBetterToml.schema.associations": {
    "^(.*/)?fatou\\.toml$": "https://fatou.dev/fatou.schema.json"
  }
}
```

### Inline Schema Directive

Editors that recognize Taplo's schema directive can select the schema from the
configuration file itself:

```toml
#:schema https://fatou.dev/fatou.schema.json
```

Other editors and language servers, including Helix, Neovim with `taplo-lsp`,
Zed, and IntelliJ, can use the same URL through their TOML schema association
settings.

## Where Fatou Looks for a Config

For a given file, Fatou walks up from the file's directory through its
ancestors, and uses the first `fatou.toml` it finds. The usual layout is a
single `fatou.toml` beside `.git` at the root of the project.

The search stops at the repository root. See the full [resolution
order](#resolution-order) below for discovery rules and fallback paths.

### User-Wide Defaults

If you want the same settings across projects, do not put a `fatou.toml` above
your repositories; use a global config instead. When no project `fatou.toml` is
found, Fatou looks for one in your user config directory, typically
`~/.config/fatou/fatou.toml`.

To use a config on a synced drive, set `FATOU_CONFIG` to its absolute path. A
non-empty value shadows the global config entirely. An unset or empty value uses
the normal user config location. Relative paths resolve from the process's
working directory; use an absolute path in editor settings.

In VS Code, add this to your user `settings.json`, then run **Fatou: Restart
Server**:

```json
{
  "fatou.serverEnv": {
    "FATOU_CONFIG": "P:/Softwares/fatou.toml"
  }
}
```

Project configs take precedence over user-wide defaults.

A missing or malformed config file makes the CLI fail. The language server logs
a configuration warning in the Fatou output channel and uses editor settings or
built-in defaults. Fix the file and restart the server to apply it.

The language server uses the same discovery rules as the CLI. It watches project
`fatou.toml` files and files reached through `extend`. An edit to the primary
global or `FATOU_CONFIG` file is picked up when the server restarts.

### Extending Another Config

Use `extend` to inherit settings from a shared file and override only the keys
this project needs:

```toml
extend = "../shared/fatou.toml"

[format]
line-width = 100
```

The path is relative to this config file. See the reference for [inheritance and
merge rules](../reference/configuration.md#extending-another-config).

### Bypassing Discovery

On the command line, `--config <PATH>` loads an explicit file and skips
discovery altogether, and `--no-config` ignores every file (project,
`FATOU_CONFIG`, and global) and runs with the built-in defaults.

### Resolution Order

Fatou uses the first source that applies:

1. `--config <PATH>`, which loads that file and skips discovery.
2. `--no-config`, which ignores every file and uses the built-in defaults.
3. The nearest `fatou.toml`, found by walking up from the file's directory. The
   walk stops at the repository root (the directory holding `.git`, whether a
   directory or a file), inclusive. This also bounds worktrees and submodules. A
   directory with no `.git` ancestor is walked to the filesystem root.
4. `$FATOU_CONFIG`, when set and non-empty. A missing or malformed file here is
   a configuration error.
5. The global user config: the first existing file in the platform's search
   order below.
6. The built-in defaults.

Fatou selects one source without merging it with the other fallbacks. A config
inherits another file only through an explicit `extend`.

For the global config, Fatou checks these paths in order:

  | Platform | First                                       | Second                       | Third                                            |
  | -------- | ------------------------------------------- | ---------------------------- | ------------------------------------------------ |
  | Linux    | `$XDG_CONFIG_HOME/fatou/fatou.toml`, if set | `~/.config/fatou/fatou.toml` | —                                                |
  | macOS    | `$XDG_CONFIG_HOME/fatou/fatou.toml`, if set | `~/.config/fatou/fatou.toml` | `~/Library/Application Support/fatou/fatou.toml` |
  | Windows  | `$XDG_CONFIG_HOME/fatou/fatou.toml`, if set | `%APPDATA%/fatou/fatou.toml` | `~/.config/fatou/fatou.toml`                     |

## Checking standalone scripts

The default `entry-points` list contains `test/runtests.jl`, `docs/make.jl`, and
`deps/build.jl`. Fatou discovers these files relative to Julia projects above
the lint target or open document, stopping at the repository boundary. Missing
default files are ignored. Setting `entry-points` replaces the default list;
`entry-points = []` disables it. `extend-entry-points` adds paths to either the
defaults or an explicit replacement list. Benchmark files and arbitrary
include-graph roots are not inferred.

Add files you run as independent programs to check undefined names across their
static includes:

```toml
[project]
extend-entry-points = ["scripts/**/*.jl"]
```

The CLI and language server enable `undefined-name` by default for each entry
point and its included files. An explicit `[lint] select` replaces those
defaults; `ignore = ["undefined-name"]` disables the rule. Files outside these
programs keep their usual defaults.

No Julia package is required for explicit paths. Both options accept literal
files and globs relative to the configuration file. Globs honor `.gitignore`;
unmatched globs add no files. Missing explicit literal paths are errors. If
`main.jl` includes `settings.jl` and `worker.jl`, functions in `worker.jl` can
use globals defined in `settings.jl`. Misspellings still produce warnings.
Separate entry points do not share globals, even when they include the same
helper file. Findings identify the entry point and module where the name is
undefined.

Run `fatou lint .` to check the discovered Julia files. Running
`fatou lint scripts/main.jl` reports findings only in that file; its includes
still supply resolution context. The language server also uses unsaved changes
in included files.

Fatou never executes Julia to analyze a script. Dynamic includes and broken
dependencies can limit undefined-name checking. See the [project
reference](../reference/configuration.md#project) for analysis limits,
`analysis-incomplete` notices, and missing-include errors.

## Excluding files

Keep vendored code out of directory scans:

```toml
exclude = ["vendored/"]
```

Use `extend-exclude` to add patterns to an inherited list. Files named
explicitly on the command line are still processed; pass `--force-exclude` to
exclude them too. See the [top-level key
reference](../reference/configuration.md#top-level-keys) for pattern roots and
per-run overrides.

## Formatting

Set only the formatting options you want to change. For example, to allow longer
lines and always write Unix line endings:

```toml
[format]
line-width = 100
line-ending = "lf"
```

See the [format reference](../reference/configuration.md#format) for defaults,
line-ending choices, command-line overrides, and deprecated key spellings.

## Choosing Lint Rules

By default most rules run; the [rule reference](../reference/rules.md) lists the
few that are opt-in for loose files. Workspace package files also enable
project-resolution rules, and declared script programs enable `undefined-name`.
Use `extend-select` to enable additional rules while keeping the defaults. For
example, to report undefined names in self-contained files without configuring
entry points or losing unused-variable warnings:

```toml
[lint]
extend-select = ["undefined-name"]
```

Use `select` to replace the defaults and `ignore` to turn individual rules off.
An explicit `select` also replaces defaults enabled by package or script
context. See the [lint reference](../reference/configuration.md#lint) for
selection order and empty selections.

`[lint.severity]` changes how loudly a rule reports, without changing whether it
runs. Rules you do not list keep their default severity:

```toml
[lint.severity]
unused-binding = "warning"
undefined-name = "error"
```

## Tuning a Rule

A rule with a tunable knob reads it from its own table, named after the rule ID.
For example,
[`discouraged-function`](../reference/rules.md#discouraged-function) ships a
deny-list of Base functions with process-wide or memory-unsafe effects, and you
can add your own entries to it:

```toml
[lint.rules.discouraged-function]
extend-functions = { sleep = "use a timer instead of blocking the task" }
```

Rules without options have no table. The [configuration
reference](../reference/configuration.md#lintrulesid) lists the rules that do.
