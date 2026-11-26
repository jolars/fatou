# Getting started

Run Fatou from the root of an existing Julia project. The commands below use the
default settings unless your project already has a `fatou.toml`.

## Install Fatou

Use the installer to download Fatou for your platform:

::: code-group

```sh [macOS/Linux]
curl --proto '=https' --tlsv1.2 -sSf https://fatou.dev/install | sh
```

```powershell [Windows]
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://fatou.dev/install.ps1 | iex"
```

:::

See [Installation](../installation.md) for package managers and other options,
or [Editor setup](editors.md) to use Fatou directly in your editor.

## Check your project {#first-run}

Check formatting and report lint findings without changing files:

```sh
fatou format --check .
fatou lint .
```

The formatter prints diffs for files that need formatting. The linter reports
findings with source locations, rule IDs, and explanations. Formatting changes,
lint findings, and parse errors cause a nonzero exit status, so these commands
can gate CI too.

Directory walks honor `.gitignore` and the project's configured exclusions. Pass
a file or subdirectory instead of `.` to narrow the scope.

## Apply changes

Apply available safe fixes, then format the result:

```sh
fatou lint --fix .
fatou format .
```

Some findings need manual changes. Review the diagnostics and the diff, then
rerun the checks above. Fix parse errors before expecting lint rules to run on a
file.

See [Linting](linting.md) for rule selection and suppressions, and
[Formatting](formatting.md) for standard input and other options.

## Share project settings

Fatou works without configuration. To customize the defaults, create a
`fatou.toml` at your project root. For example:

```toml
[format]
line-width = 100

[lint]
ignore = ["unused-argument"]
```

The CLI and language server use the same project configuration. See
[Configuration](configuration.md) for common settings and discovery rules,
including how to declare entry points for standalone scripts spread across
included files.

## Use Fatou in your workflow

Follow [Editor setup](editors.md) for formatting, diagnostics, and navigation
while editing. See [Integrations](integrations.md) to run the checks in CI or a
pre-commit hook.

<span id="cargo"></span> <span id="homebrew"></span> <span id="npm"></span>
<span id="pypi"></span> <span id="aur"></span> <span id="nix"></span>
<span id="mise-and-aqua"></span> <span id="install-script"></span>
<span id="prebuilt-binaries"></span> <span id="from-source"></span>
<span id="dprint"></span>
