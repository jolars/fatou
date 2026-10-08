# Linting

Fatou checks Julia source without executing it. After
[installation](getting-started.md#installation), run the linter from your
project directory.

## Check Files

Check one file or a directory of Julia files:

```sh
fatou lint src/example.jl
fatou lint .
```

Directory walks honor `.gitignore` and the exclusions in `fatou.toml`.
Explicitly named files are still checked unless you add `--force-exclude`. See
[excluding files](configuration.md#excluding-files).

Findings include a source location, rule ID, and explanation. Lint findings and
parse errors cause a nonzero exit status, so the command can gate CI. Fix parse
errors before expecting lint rules to run on that file. Informational
`analysis-incomplete` notices explain skipped analysis and do not fail the
check.

For compact output or machine-readable diagnostics:

```sh
fatou lint --output concise .
fatou lint --output json .
```

## Choose Rules

The [Lint Rules](../reference/rules.md) reference lists each rule and whether it
runs by default. Change the selection in `fatou.toml`:

```toml
[lint]
extend-select = ["undefined-name"]
ignore = ["unused-argument"]
```

`extend-select` adds rules while preserving the default selection. `select`
replaces that selection, and `ignore` removes rules. See
[Configuration](configuration.md#choosing-lint-rules) for severity overrides and
rule-specific options.

Rules that resolve names need project context. For standalone programs spread
across included files, declare their entry points:

```toml
[project]
entry-points = ["scripts/main.jl"]
```

This enables undefined-name checking for the program and its static includes.
Run `fatou lint .` to report findings throughout the discovered files; linting
only the entry-point file uses includes as context but reports findings only in
that file. See [checking standalone
scripts](configuration.md#checking-standalone-scripts) for the limits of static
analysis.

## Apply Fixes

Apply safe fixes in place, then format the rewritten code:

```sh
fatou lint --fix .
fatou format .
```

Review the changes and remaining findings. A fix may change code without
choosing its final layout, so formatting runs separately afterward. Some
findings require manual edits.

To also apply fixes marked unsafe, use `fatou lint --fix --unsafe-fixes .`.
These can change behavior and need review. The rule reference describes the
fixes each rule offers.

## Suppress a Finding

Put a suppression comment before the code it applies to:

```julia
# fatou-ignore unused-argument: the callback requires this signature
on_event(event) = nothing
```

`# fatou-ignore <rule>: <reason>` suppresses that rule on the next statement or
construct, including its body. To suppress a named rule throughout a file, use
`# fatou-ignore-file <rule>: <reason>`. Give each rule its own directive; rule
names are not comma-separated.

These are line comments, not `#= ... =#` block comments. They suppress lint
findings without disabling formatting, and they cannot suppress parse errors.
For a rule the entire project should disable, use `[lint] ignore` instead.

See [Editor Setup](editors.md) for diagnostics and fixes while editing, and
[Integrations](integrations.md) for automated checks.
