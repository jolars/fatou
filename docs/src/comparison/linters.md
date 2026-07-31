# Linters

Fatou ships a linter that flags likely mistakes—unused bindings, undefined
names, arity errors, and more (see the [rule reference](../reference/rules.md)).
Julia's other linting comes bundled inside the language servers, not as
standalone tools: [LanguageServer.jl](language-servers.md) carries the checks
formerly known as [StaticLint.jl](https://github.com/julia-vscode/StaticLint.jl)
(now vendored into JuliaWorkspaces.jl), and [JETLS.jl](language-servers.md)
produces diagnostics in three stages, culminating in real type inference via
[JET.jl](https://github.com/aviatesk/JET.jl). This page compares Fatou's linting
against those two. (The formatters
[Runic](https://github.com/fredrikekre/Runic.jl) and
[JuliaFormatter](https://github.com/domluna/JuliaFormatter.jl) do not lint, so
they are not in scope here—see [Formatters](formatters.md) for those.)

As with the [language servers](language-servers.md) and
[formatters](formatters.md), the defining difference is that Fatou **does not run
Julia**. A linter that runs the compiler can know things a static one cannot,
above all types. What Fatou offers in return is a fast, dependency-free,
Ruff-style linter that is deliberately conservative about what it flags.

## At a glance

| | **Fatou** | **LanguageServer.jl** (StaticLint) | **JETLS.jl** |
|---|---|---|---|
| Implementation language | Rust | Julia | Julia |
| Requires a Julia install | No | Yes | Yes |
| Analysis model | Static: syntax + name/scope resolution | Static + runtime symbol index of deps | Type inference (Julia compiler) |
| Type-based diagnostics | No | Limited (best-effort) | Yes (field access, bounds, method errors) |
| Cross-package symbol knowledge | Base/Core snapshot + workspace | Full (indexes installed deps) | Full (loads code) |
| False-positive stance | Conservative: unknown silences the check | Known `MissingReference` false positives | Inference-backed |
| Rule model | Named rules, categories, `select`/`ignore`, per-rule severity | `julia.lint.*` toggles | Diagnostic stages |
| Autofix | Yes (safe / unsafe) | Some quick fixes | Some code actions |
| Standalone / CLI | Yes (`fatou lint`, incl. JSON output) | No (inside the server) | `jetls check` CLI |
| License | MIT | MIT | MIT |

## The core distinction: what a linter can know without running Julia

Julia is dynamic, and much of what you might want a linter to catch is only
knowable at runtime: the concrete type of a value, which method a call
dispatches to, whether a field exists on a struct. A linter that runs inside a
Julia process—as both LanguageServer.jl and JETLS do—can reach for that
information. JETLS goes furthest, using the compiler's own type inference to flag
genuine type errors (accessing a non-existent field, indexing out of bounds,
calling a method that does not exist for the inferred argument types). Fatou
cannot do any of this, and does not try. Its analysis is purely static: it
resolves names and scopes over the syntax tree.

This shapes what Fatou's rules can soundly check. Rather than guess in the
absence of type or full dependency information, Fatou's resolution-dependent
rules **fail safe: when Fatou cannot be sure, it stays silent.**

## The false-positive trade-off

This conservatism is a direct response to the best-known weakness of the
Julia-native static linter: StaticLint's `MissingReference` check has a
long-standing reputation for false positives on valid `using`/import code,
flagging names that are in fact perfectly defined.

Fatou's [`undefined-name`](../reference/rules/undefined-name.md) rule is built to
avoid exactly this. It resolves a name against a series of tiers—locals, file
bindings, workspace siblings, whole-module `using` exports, and a Base/Core
snapshot—and flags an identifier only when *no* tier provides it. Whenever the
file does something that could make any name valid (an `eval`, an `include`
outside a known workspace, or a `using` of a module Fatou cannot resolve), the
rule skips the file entirely rather than risk a false positive. The
[`call-arity`](../reference/rules/call-arity.md) rule is equally cautious: an
unknown method always *silences* the check rather than triggering it.

Because these rules need project context to be sound, they are **off by default
in the CLI** (which resolves only against the built-in Base/Core snapshot) and
enabled by the language server for workspace member files, where the surrounding
project gives them enough to work with.

The trade-off is honest: Fatou will miss some real problems that a
type-and-symbol-aware linter would catch, in exchange for rarely crying wolf.

## LanguageServer.jl (StaticLint)

The linting in [LanguageServer.jl](https://github.com/julia-vscode/LanguageServer.jl)
is genuine cross-file static analysis. Because it maintains a runtime-built index
of your installed dependencies' symbols, it can check things Fatou cannot—most
notably **type piracy** (extending a method you own on types you do not),
which requires knowing where types and functions are defined across packages.

Its check families overlap substantially with Fatou's rule set, though:
missing references, unused bindings, unused function arguments, incorrect call
arguments, `nothing` comparisons, constant conditionals, include loops, module
naming, and unused type parameters all have counterparts on both sides. The
differences are the runtime dependency, the symbol index (which enables the
piracy check but also drives the indexing latency and false-positive issues), and
packaging: StaticLint's checks are configured through `julia.lint.*` editor
settings and run inside the server, not as a standalone tool with its own CLI.

## JETLS.jl

[JETLS.jl](https://github.com/aviatesk/JETLS.jl) represents the deepest linting
of the three. Its diagnostics come in three escalating stages:

1. **Syntax** errors from JuliaSyntax.
2. **Lowering**-stage checks: undefined and unused bindings, unreachable code,
   scope ambiguities, and import issues—the category Fatou's static rules also
   occupy.
3. **Type inference** via JET.jl: non-existent field access, out-of-bounds
   indexing, method errors, and non-boolean conditions.

That third stage is the whole point of JETLS and is precisely what a
non-Julia-running tool cannot replicate. JETLS runs the lighter checks live as
you type and defers the expensive inference-backed pass to save, reflecting its
cost. Fatou's static rules run instantly on every keystroke because they never
had to pay that cost in the first place.

## Fatou's rule model

Where Fatou's linter distinguishes itself in *ergonomics* is a design borrowed
from [Ruff](https://docs.astral.sh/ruff/) rather than from the Julia ecosystem:

- **Named rules in categories.** Every rule has a stable ID
  (`unused-binding`, `call-arity`, …) and lives in a category (`correctness`,
  `suspicious`). Each is documented with a worked example whose diagnostics are
  rendered by running the linter itself, so the docs cannot drift from behavior.
- **Selectable and configurable.** `[lint]` `select`/`ignore` turn rules on and
  off by ID, and `[lint.severity]` overrides the severity a rule reports.
- **Autofix with a safety model.** `fatou lint --fix` applies safe fixes;
  `--unsafe-fixes` opts into the rest. Fixes are byte-range replacements, kept
  deliberately separate from the formatter (run `fatou format` afterward for
  canonical layout).
- **Standalone and CI-friendly.** `fatou lint` is a real CLI with `pretty`,
  `concise`, and `json` output, so it drops into CI and pre-commit without a
  Julia toolchain—the linting equivalent of the formatter's CI story.

## Where Fatou fits

Fatou's linter is the fast, portable, low-noise option: a curated catalog of
static rules with Ruff-like ergonomics, no Julia runtime, and a bias toward
silence when it cannot be certain. It will not catch type errors or type
piracy—those need a running compiler and a full symbol index, which is exactly
what JETLS and LanguageServer.jl bring. As with the other tools, the pairing is
natural: run Fatou for instant, dependency-free static checks everywhere,
including CI, and lean on a Julia-native server for the deeper, type-aware
diagnostics when you have a Julia environment in hand.
