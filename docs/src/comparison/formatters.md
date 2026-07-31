# Formatters

Fatou's formatter competes with two established Julia-native tools:
[Runic.jl](https://github.com/fredrikekre/Runic.jl), a zero-configuration
formatter with "rules set in stone," and
[JuliaFormatter.jl](https://github.com/domluna/JuliaFormatter.jl), the highly
configurable, multi-style tool that has long been the de-facto standard. This
page explains how Fatou's formatting model differs from theirs.

Two things set Fatou apart. First, as with the [language
servers](language-servers.md), Fatou is a compiled Rust binary that **does not
require Julia**. Second, and more distinctive among formatters, Fatou is built
around **deterministic full reflow**: the output is decided solely by Fatou's
rules and a target line width, never by how the input happened to be laid out.

## At a glance

| | **Fatou** | **Runic.jl** | **JuliaFormatter.jl** |
|---|---|---|---|
| Implementation language | Rust | Julia | Julia |
| Requires a Julia install | No | Yes | Yes |
| Configuration | Minimal (width, indent, line ending) | None ("rules set in stone") | Extensive (~38 options, `.JuliaFormatter.toml`) |
| Named styles | One | One | Default, Blue, YAS, SciML, Minimal |
| Line-width limit / wrapping | Yes (default 92) | No line-width limit at all | Yes (`margin`, default 92) |
| Reflow model | Full reflow, always | Preserves author line breaks | Full reflow by default; can honor source (`join_lines_based_on_source`) |
| Output depends on input layout | Never (by design) | Yes (author's breaks are load-bearing) | No by default; yes if source-honoring enabled |
| License | MIT | MIT | MIT |

## The core distinction: how line breaks are decided

The deepest difference between these tools is not *what* they format but *what
decides where the line breaks go*.

Fatou **fully reflows** every construct. It lays each one out from scratch under
the configured `line-width`, breaking only where width or semantics require it,
regardless of where the source broke. The input's line breaks, whitespace,
operator spelling, and numeric-literal form never influence the result. This is
[Tenet 1](https://github.com/jolars/fatou/blob/main/AGENTS.md) of the project:
semantically-equivalent inputs *must* format identically.

A single example captures the whole difference. Given this input:

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

Fatou **collapses** the short call that the author had split across lines, and
**breaks** the long call that the author had left on one line—each decision made
purely by measuring against the width. Runic would do the opposite on both: it
keeps `foo` expanded because the author "committed" to multiple lines, and
leaves `bar` on one long line because Runic has no line-width limit and never
breaks a line for width. JuliaFormatter, with its default settings, behaves like
Fatou here (it too is width-driven); the difference from JuliaFormatter is about
determinism and configuration rather than the reflow model, as explained below.

### Why this matters

Because Fatou's output is a pure function of the source's *meaning* and the
width setting, the layout of the input is irrelevant. You cannot coax a
different result by hand-arranging your code, there are no "magic comma" tricks
to memorize, and re-running the formatter on differently-formatted but
equivalent code always converges to the same output. That predictability is the
point.

## Runic.jl

[Runic.jl](https://github.com/fredrikekre/Runic.jl), by Fredrik Ekre, shares
Fatou's *opinionated, deterministic* spirit. Modeled on `gofmt`, it has **no
configuration at all**: indentation is fixed at four spaces and there are no
style knobs. Its output is stable and pinnable per version by an explicit
policy. In that respect Runic and Fatou agree: formatting should not be a matter
of taste to be re-litigated per project.

Where they part ways is reflow. **Runic has no line-width limit** and does not
break long lines or join short ones for width—its own docs put it bluntly: "Line
width limit: No. Use your Enter key or refactor your code." Runic normalizes
*how* a construct looks once the author has broken it across lines (indentation,
trailing commas for array and tuple literals, blank lines), but the decision of
single-line versus multi-line is the author's, encoded in the source's existing
line breaks. Fatou takes that decision away and makes it from the width.

Both tools go beyond whitespace to normalize surface syntax, and here they
overlap heavily. Like Runic, Fatou normalizes numeric literals (`1.` → `1.0`,
`.5` → `0.5`), operator spacing (`a*b` → `a * b`), `for` iteration syntax
(`for i = 1:2` and `for j ∈ 1:2` → `for … in …`), and `where` clauses
(`where T` → `where {T}`). One notable difference: Runic inserts explicit
`return` statements into function bodies; Fatou does not.

**Choose Runic when** you want a zero-config, deterministic formatter and you
prefer to control line breaks by hand rather than have them reflowed to a width
(and you already run Julia).

## JuliaFormatter.jl

[JuliaFormatter.jl](https://github.com/domluna/JuliaFormatter.jl), by Dominique
Luna, is the opposite philosophy: "an opinionated code formatter for Julia—plot
twist, the opinion is your own." It is **highly configurable** (around 38
options, read from a `.JuliaFormatter.toml`) and ships several named styles
(Default, Blue, YAS, SciML, Minimal). Its width-driven reflow, governed by a
`margin` option, is the closest of the three to Fatou's default behavior.

The differences are determinism and surface area:

- **Configuration.** JuliaFormatter lets each project pick a style and tune
  dozens of options, including AST-changing transforms (`import_to_using`,
  short-vs-long function-def conversion, `always_use_return`, and more). Fatou
  deliberately exposes only `line-width`, `indent-width`, and `line-ending`;
  everything else is fixed by the rules. Fatou trades configurability for a
  single canonical style.
- **Source dependence.** JuliaFormatter fully reflows by default, but it offers
  `join_lines_based_on_source`, which makes the output honor the source's line
  breaks. Fatou has no such option and never honors source breaks—doing so would
  violate Tenet 1.
- **Determinism and idempotency.** JuliaFormatter's flexibility has a cost:
  certain option combinations (notably source-honoring modes) have historically
  produced idempotency bugs, where re-formatting already-formatted code changes
  it again. Fatou's design makes idempotency structural rather than
  configuration-dependent, and it is enforced in the test suite: a dedicated
  test runs `format(format(x)) == format(x)` over every fixture.

**Choose JuliaFormatter when** you want fine-grained control over style, a
specific named style (Blue, YAS, SciML), or AST-level rewrites—and you are
comfortable with a Julia dependency and the configuration surface that comes with
it.

## Where Fatou fits

Fatou aims to be the fast, portable, low-configuration choice: opinionated and
deterministic like Runic, width-reflowing like JuliaFormatter's default, but
requiring no Julia runtime and offering essentially no style knobs to argue
over. Its guarantees are the selling point—identical output for equivalent
inputs, structural idempotency, and a single canonical style—delivered by a
self-contained binary that runs anywhere, including CI and pre-commit with no
Julia setup step.

For raw formatting throughput against Runic and JuliaFormatter, including
cold-start numbers, see [Performance](../performance.md).
