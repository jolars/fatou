# Language Servers

Fatou is not the only language server for Julia. The two most prominent
Julia-native options are
[LanguageServer.jl](https://github.com/julia-vscode/LanguageServer.jl), the
long-standing backend of the official VS Code extension, and
[JETLS.jl](https://github.com/aviatesk/JETLS.jl), a newer, compiler-powered
server. This page explains how Fatou differs from them and when you might reach
for one over another.

The largest divisor is that Fatou is the only one of the three that does not run
Julia, which explains the differences in startup, dependency handling, and type
awareness.

## At a Glance

  |                          | **Fatou**                              | **LanguageServer.jl**                          | **JETLS.jl**                                   |
  | ------------------------ | -------------------------------------- | ---------------------------------------------- | ---------------------------------------------- |
  | Implementation language  | Rust                                   | Julia                                          | Julia                                          |
  | Requires a Julia install | No                                     | Yes                                            | Yes                                            |
  | Runs your code           | No                                     | Loads dependency symbols in a Julia subprocess | Yes (compiler-driven)                          |
  | Analysis model           | Static: syntax + name/scope resolution | Static analysis + runtime symbol indexing      | Type inference via the Julia compiler (JET.jl) |
  | Type-aware diagnostics   | No                                     | Best-effort; deeper via optional JET           | Yes, core feature                              |
  | Formatter                | Built in (own engine)                  | Delegates to JuliaFormatter / Runic            | Delegates to Runic / JuliaFormatter            |
  | Linter                   | Built in                               | Built in (StaticLint)                          | Built in (lowering + type checks)              |
  | First-run cost           | None (single binary)                   | Precompilation + package indexing (minutes)    | Precompilation (one-time)                      |
  | Maturity                 | Young, focused                         | Mature, de-facto default                       | Experimental, actively developed               |
  | License                  | MIT                                    | MIT                                            | MIT                                            |

## The Core Distinction: Running Julia or Not

Both LanguageServer.jl and JETLS.jl are themselves Julia packages. They start a
Julia process and analyze your code from inside a live runtime. That gives them
access to information that only exists at runtime—above all, **types**—but it
also means they inherit Julia's startup and compilation characteristics.

Fatou takes the opposite approach. It is a single compiled Rust binary that
never starts Julia. It parses and analyzes source text directly, the way
rust-analyzer analyzes Rust or `gopls` analyzes Go. Everything Fatou knows, it
learns by reading code, not by running it.

This is a genuine trade-off, not a strict improvement, and it cuts both ways.

**What Fatou gains by not running Julia:**

- **No Julia dependency.** Fatou works in a repository, a CI job, or an editor
  with no Julia toolchain installed at all.
- **Instant startup.** There is no precompilation step and no package-indexing
  phase. LanguageServer.jl's first-run indexing of a project's dependencies is a
  well-known pain point that can take minutes (and, for large dependency trees,
  much longer); Fatou has no equivalent phase.
- **Predictable, low resource use.** No JIT, no resident Julia session, no
  symbol cache to build and hold in memory.
- **Deterministic behavior.** Results depend only on the source text, not on
  which packages happen to be installed or precompiled in the active
  environment.

**What Fatou gives up by not running Julia:**

- **No type inference.** Fatou cannot tell you the inferred type of an
  expression, and it cannot produce diagnostics that depend on types (method
  errors, non-existent field access, and so on). This is precisely what JETLS is
  built to do.
- **Limited cross-package knowledge.** Fatou reasons about the code it can read.
  It does not load your dependencies to discover the exported symbols, methods,
  and docstrings of arbitrary packages the way LanguageServer.jl's symbol
  indexing does.

If your workflow depends on type-driven analysis of a fully instantiated
project, a Julia-native server will tell you things Fatou structurally cannot.
If you want fast, dependency-free formatting, linting, and navigation that works
anywhere, that is exactly what Fatou is for.

## LanguageServer.jl

[LanguageServer.jl](https://github.com/julia-vscode/LanguageServer.jl) is the
mature, de-facto standard. It is the backend of the official [Julia VS Code
extension](https://www.julia-vscode.org) and is reused by many other editors
over LSP.

Interestingly, its modern architecture is a close cousin of Fatou's. As of its
v5 rewrite, the analysis engine is
[JuliaWorkspaces.jl](https://github.com/julia-vscode/JuliaWorkspaces.jl) built
on [Salsa.jl](https://github.com/RelationalAI-oss/Salsa.jl)—an incremental,
memoized query system, the same idea Fatou gets from Rust's
[`salsa`](https://crates.io/crates/salsa). Both also target the same parser
grammar: LanguageServer.jl parses with
[JuliaSyntax](https://github.com/JuliaLang/JuliaSyntax.jl) (now Julia's standard
parser), and Fatou's own Rust parser is written to match JuliaSyntax's output.

Where they diverge is the runtime. LanguageServer.jl still needs an installed
Julia, and it discovers the symbols of your dependencies by inspecting them in a
spawned Julia subprocess, caching the results on disk. That symbol index is what
powers completion and hover for third-party packages—and it is also the source
of its slow first-run indexing and memory-use complaints. Its linting is genuine
cross-file static analysis (the former StaticLint.jl, now vendored into
JuliaWorkspaces), and formatting is delegated to
[JuliaFormatter.jl](https://github.com/domluna/JuliaFormatter.jl) (default) or
[Runic.jl](https://github.com/fredrikekre/Runic.jl).

**Choose LanguageServer.jl when** you want the established, batteries-included
default inside VS Code, with symbol knowledge of your installed dependencies,
and you already have a Julia environment set up.

## JETLS.jl

[JETLS.jl](https://github.com/aviatesk/JETLS.jl) is the next-generation entrant,
by Shuhei Kadowaki (author of [JET.jl](https://github.com/aviatesk/JET.jl)). Its
premise is to use the *actual Julia compiler* for analysis: type inference via
JET.jl, macro-aware navigation via
[JuliaLowering.jl](https://github.com/c42f/JuliaLowering.jl), and parsing via
JuliaSyntax. This lets it offer things the others cannot—type on hover, inlay
type hints, argument-type-aware completion, and type-based diagnostics such as
detecting non-existent field access or out-of-bounds indexing.

That power is the point of maximal contrast with Fatou. JETLS is the deepest,
most semantically aware of the three; Fatou is the lightest and most portable.
JETLS runs your code to understand it; Fatou never does.

As of 2026, JETLS is explicitly **experimental and not production-ready** (its
own README says so), it requires a recent Julia (1.12+), and it is essentially a
single-author project under very active development. It is being integrated into
the Julia VS Code extension and is widely seen as the future direction of
compiler-powered Julia tooling, though nothing formally declares it the
committed replacement for LanguageServer.jl.

**Choose JETLS when** you want cutting-edge, type-aware analysis and are
comfortable running experimental tooling on a current Julia release.

## Where Fatou fits

Fatou does not try to out-analyze a server that has the Julia compiler in
hand—it cannot, and it deliberately does not try. Instead it aims to be the
fast, portable, zero-dependency choice for the large amount of useful work that
does not require running Julia: deterministic formatting, static linting, and
syntactic navigation (completion, hover, go-to-definition, references, rename,
symbols, and the rest of the LSP surface).

Because Fatou is a self-contained binary, it fits naturally in places a
Julia-native server is awkward:

- **CI**, via [fatou-action](https://github.com/jolars/fatou-action) or
  [pre-commit](https://pre-commit.com), with no Julia setup step.
- **Editors** where you want instant, always-available formatting and linting
  regardless of the project's Julia environment.
- **Constrained or ephemeral environments** (containers, remote sessions) where
  installing and precompiling Julia is expensive or impossible.

The three are not mutually exclusive. Running Fatou alongside a Julia-native
server is a reasonable setup: Fatou handles formatting and fast static checks,
while the Julia-native server contributes the type-aware analysis that only a
running compiler can provide.

For the formatting-speed side of this comparison—including cold-start numbers
against the Julia formatters—see [Performance](../performance.md).
