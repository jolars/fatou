# Comparison

Fatou overlaps with several Julia tools: the language servers
[LanguageServer.jl](https://github.com/julia-vscode/LanguageServer.jl) and
[JETLS.jl](https://github.com/aviatesk/JETLS.jl), and the formatters
[Runic.jl](https://github.com/fredrikekre/Runic.jl) and
[JuliaFormatter.jl](https://github.com/domluna/JuliaFormatter.jl).

The two servers also carry linters,
[JuliaWorkspaces.jl](https://github.com/julia-vscode/JuliaWorkspaces.jl) in the
case of LanguageServer.jl and [JET.jl](https://github.com/aviatesk/JET.jl) in
the case of JETLS.jl. Although some of the linting is done inside the servers in
both cases.

The primary difference with respect to the tools above is that Fatou is a
compiled Rust binary that never starts Julia. It reads your code the way
rust-analyzer reads Rust, rather than loading it into a running session.
Everything below follows from that. All of these tools are MIT-licensed.

## Language Servers

  |                          | Fatou                 | LanguageServer.jl                      | JETLS.jl                             |
  | ------------------------ | --------------------- | -------------------------------------- | ------------------------------------ |
  | Requires a Julia install | No                    | Yes                                    | Yes                                  |
  | Analysis model           | Syntax and name/scope | Static analysis + runtime symbol index | Type inference (JET.jl)              |
  | Type-aware diagnostics   | No                    | Best-effort                            | Yes                                  |
  | Formatter                | Built in              | Delegates to JuliaFormatter or Runic   | Delegates to Runic or JuliaFormatter |
  | Linter                   | Built in              | Built in (StaticLint)                  | Built in (lowering + type checks)    |
  | First-run cost           | None                  | Precompilation and package indexing    | Precompilation                       |
  | Maturity                 | Young                 | Mature, de-facto default               | Experimental                         |

Not running Julia means no toolchain to install, no precompilation, and no
package-indexing phase before the server is useful. It also means results depend
only on the source text, not on which packages happen to be precompiled in the
active environment.

The cost is types. Fatou cannot tell you the inferred type of an expression or
flag anything that depends on one, and it does not load your dependencies to
learn their exported symbols, methods, and docstrings.

### LanguageServer.jl

The mature default, and the backend of the [Julia VS Code
extension](https://www.julia-vscode.org). Its architecture is closer to Fatou's
than you might expect: since the v5 rewrite the analysis engine is
[JuliaWorkspaces.jl](https://github.com/julia-vscode/JuliaWorkspaces.jl) on top
of [Salsa.jl](https://github.com/RelationalAI-oss/Salsa.jl), the same
incremental query model Fatou gets from Rust's
[`salsa`](https://crates.io/crates/salsa), and it parses with
[JuliaSyntax](https://github.com/JuliaLang/JuliaSyntax.jl), which Fatou's parser
is written to match.

The runtime is where they part. LanguageServer.jl discovers your dependencies'
symbols by inspecting them in a spawned Julia process and caching the results.
That index is what powers completion and hover for third-party packages, and it
is also why the first run on a large project can take minutes.

### JETLS.jl

A language server made by the author of
[JET.jl](https://github.com/aviatesk/JET.jl), and built on the actual compiler:
type inference through JET.jl, macro-aware navigation through
[JuliaLowering.jl](https://github.com/c42f/JuliaLowering.jl). It offers what the
others cannot, including types on hover, inlay type hints, and diagnostics for
non-existent field access or out-of-bounds indexing. Inferred types are the
dividing line: Fatou does show inlay hints, but only for facts it can read off
disk, such as a dependency's resolved version in a `Project.toml`.

It is also the furthest from Fatou. As of 2026 its README calls it experimental
and not production-ready, it needs Julia 1.12 or newer, and it is under heavy
development. It is being integrated into the Julia VS Code extension.

## Formatters

  |                                | Fatou                      | Runic.jl                      | JuliaFormatter.jl                                |
  | ------------------------------ | -------------------------- | ----------------------------- | ------------------------------------------------ |
  | Requires a Julia install       | No                         | Yes                           | Yes                                              |
  | Configuration                  | Width, indent, line ending | None                          | ~38 options, `.JuliaFormatter.toml`              |
  | Named styles                   | One                        | One                           | Default, Blue, YAS, SciML, Minimal               |
  | Line-width limit               | Yes (default 92)           | None                          | Yes (`margin`, default 92)                       |
  | Reflow model                   | Always full reflow         | Preserves the author's breaks | Full reflow, unless `join_lines_based_on_source` |
  | Output depends on input layout | Never                      | Yes                           | Only with source-honoring enabled                |

The interesting difference is what decides where the line breaks go. Given this
input:

```julia
foo(
  a,
  b,
)
bar(aaaaaaaaaaaaaaaa, bbbbbbbbbbbbbbbb, cccccccccccccccc, dddddddddddddddd, eeeeeeeeeeeeeeee)
```

Fatou produces:

```julia
foo(a, b)
bar(
    aaaaaaaaaaaaaaaa,
    bbbbbbbbbbbbbbbb,
    cccccccccccccccc,
    dddddddddddddddd,
    eeeeeeeeeeeeeeee,
)
```

It collapses the short call the author had split, and breaks the long call the
author had left on one line, purely by measuring against `line-width`. Runic
does the opposite on both: it keeps `foo` expanded because the author broke it,
and leaves `bar` alone because it has no width limit. JuliaFormatter's defaults
behave like Fatou's here.

Equivalent code formats identically under Fatou no matter how it was laid out.
There is no way to hand-arrange your source into a different result. Formatting
is also idempotent, and the test suite checks `format(format(x)) == format(x)`
over every fixture.

### Runic.jl

Modeled on `gofmt`, with no configuration at all: indentation is four spaces and
there are no style knobs. Fatou and Runic agree that formatting should not be
re-litigated per project.

They disagree about reflow. Runic has no line-width limit and never breaks or
joins lines for width; its own docs say "Line width limit: No. Use your Enter
key or refactor your code." It normalizes how a construct looks once you have
broken it, but single-line versus multi-line stays your decision. Fatou takes
that decision from the width instead.

Beyond whitespace the two overlap heavily. Both normalize numeric literals (`1.`
to `1.0`, `.5` to `0.5`), operator spacing, `for` iteration syntax (`=` and `∈`
to `in`), and `where` clauses (`where T` to `where {T}`). Runic also inserts
explicit `return` statements; Fatou does not.

Runic is the better fit if you want to place line breaks by hand and already run
Julia.

### JuliaFormatter.jl

Dominique Luna's formatter takes the opposite approach to configuration: around
38 options read from a `.JuliaFormatter.toml`, plus named styles (Default, Blue,
YAS, SciML, Minimal). Its width-driven reflow is the closest of the three to
Fatou's.

Where it goes further is control. It can convert between short and long function
definitions, rewrite `import` to `using`, add `return` statements, and honor the
source's line breaks with `join_lines_based_on_source`. Fatou exposes
`line-width`, `indent-width`, and `line-ending`, and nothing else.

Reach for JuliaFormatter when you want a specific named style or the AST-level
rewrites.

## Linters

  |                                | Fatou                                    | LanguageServer.jl (StaticLint) | JETLS.jl          |
  | ------------------------------ | ---------------------------------------- | ------------------------------ | ----------------- |
  | Requires a Julia install       | No                                       | Yes                            | Yes               |
  | Type-based diagnostics         | No                                       | Limited                        | Yes               |
  | Cross-package symbol knowledge | Base/Core snapshot + workspace           | Indexes installed dependencies | Loads code        |
  | Rule model                     | Named rules, `select`/`ignore`, severity | `julia.lint.*` toggles         | Diagnostic stages |
  | Autofix                        | Safe and unsafe fixes                    | Some quick fixes               | Some code actions |
  | Standalone CLI                 | `fatou lint`                             | No, runs inside the server     | `jetls check`     |

Fatou's rules resolve names and scopes over the syntax tree, so they stay silent
whenever certainty runs out.
[`undefined-name`](../reference/rules.md#undefined-name) resolves an identifier
against locals, file bindings, workspace siblings, whole-module `using` exports,
and a Base/Core snapshot, and flags it only when no tier provides it; if the
file calls `eval`, `include`s outside a known workspace, or `using`s a module
Fatou cannot resolve, the rule skips the file rather than guess.
[`call-arity`](../reference/rules.md#call-arity) treats an unknown method as a
reason to say nothing.

Both rules need project context to be sound, so the CLI leaves them opt-in and
the language server turns them on for workspace member files. The bargain is
that Fatou misses real problems a type-aware linter would catch, and rarely
cries wolf. StaticLint's `MissingReference` has the opposite reputation: false
positives on valid `using` and `import` code.

The check families otherwise overlap a good deal with StaticLint's: missing
references, unused bindings and arguments, incorrect call arguments, `nothing`
comparisons, constant conditionals, include loops, module naming, and unused
type parameters have counterparts on both sides. Fatou also has a
[`type-piracy`](../reference/rules.md#type-piracy) rule, which needs enough
project context to tell an owned type from a foreign one and is opt-in for the
same reason.

JETLS diagnoses in three stages: syntax errors from JuliaSyntax, lowering-stage
checks (undefined and unused bindings, unreachable code, scope ambiguities,
import issues), and type inference through JET.jl. The first two overlap with
Fatou's rules. The third does not, and cannot be replicated without running the
compiler; JETLS itself defers that pass to save, while Fatou's static rules run
on every keystroke.

Fatou's rule model is borrowed from [Ruff](https://docs.astral.sh/ruff/): stable
rule IDs in categories, `select`/`ignore` and per-rule severity in `[lint]`,
`--fix` for safe fixes with `--unsafe-fixes` for the rest, and `pretty`,
`concise`, or `json` output from the CLI. See the [rule
reference](../reference/rules.md) for the catalogue.

## Running Them Together

Fatou and a Julia-native server coexist happily: Fatou formats and runs its
static checks instantly and everywhere, including CI via
[fatou-action](https://github.com/jolars/fatou-action) or
[pre-commit](https://pre-commit.com) with no Julia setup, and the Julia server
contributes the type-aware analysis that needs a running compiler. [Editor
Setup](editors.md) shows how to give Fatou formatting while another server keeps
the rest.

For formatting throughput and cold-start numbers against Runic and
JuliaFormatter, see [Performance](performance.md).
