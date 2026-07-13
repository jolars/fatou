# `unused-argument`

Flag a function parameter that is never read in its body. Every signature form is covered — long, short (`f(x) = ...`), anonymous, and `do`. All-underscore names (`_`, `__`) follow Julia's throwaway convention and are skipped, and stub methods whose body is a single placeholder expression — a literal (`f(x) = 0`), `nothing`, or an `error(...)`/`throw(...)` call — are exempt. Because methods that dispatch on an argument's type without reading its value are common, this rule is disabled by default; enable it with `--select unused-argument`.

`factor` is accepted but never used:

```julia
function scale(x, factor)
    2 * x
end
```

```text
warning: unused-argument
 --> example.jl:1:19
  |
1 | function scale(x, factor)
  |                   ^^^^^^ function argument `factor` is never used
```
