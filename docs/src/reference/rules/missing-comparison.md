# `missing-comparison`

Flag `x == missing` / `x != missing`. `missing` propagates through `==`, so the comparison is always `missing` no matter what `x` is, and using it as a condition raises a `TypeError`. Use `ismissing` (or the identity test `===` / `!==`) instead. The rule reports an unsafe fix rewriting `==` to `===` and `!=` to `!==`: the rewrite turns a `missing` result into a `Bool`, which is the intent but is still a change in behavior.

Comparing against `missing` by value:

```julia
if x == missing
    1
end
```

```text
warning: missing-comparison
 --> example.jl:1:4
  |
1 | if x == missing
  |    ^^^^^^^^^^^^ comparison against `missing` by value is always `missing`; use `ismissing` or `===`
  = help: Replace `==` with `===`
```
