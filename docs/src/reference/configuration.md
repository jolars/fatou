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

## `[format]`

  | Key            | Type    | Default | Description                                         |
  | -------------- | ------- | ------- | --------------------------------------------------- |
  | `line-width`   | integer | `92`    | The width the formatter tries to keep lines within. |
  | `indent-width` | integer | `4`     | Number of spaces per indentation level.             |

Defaults follow common Julia conventions. Both keys can be overridden per run
with the `--line-width`/`--indent-width` flags on `fatou format`.

```toml
[format]
line-width = 92
indent-width = 4
```

> **Deprecation**: the snake_case keys `line_width` and `indent_width` are still
> accepted but print a warning. Use the kebab-case `line-width` and
> `indent-width` instead; the snake_case forms will be removed in a future
> release.

## `[lint]`

  | Key      | Type             | Default | Description                      |
  | -------- | ---------------- | ------- | -------------------------------- |
  | `select` | array of strings | unset   | If set, only these rule IDs run. |
  | `ignore` | array of strings | `[]`    | Rule IDs to disable.             |

> **Note**: no lint rules ship yet, so `[lint]` is scaffolding today. The keys
> are accepted so a `fatou.toml` can be prepared ahead of the first rules
> landing.

```toml
[lint]
select = ["some-rule"]
ignore = ["another-rule"]
```
