# `index-from-length`

Flag two suspect `for`-loop iteration specs. First, `for i in 1:length(x)` (or `1:size(x, d)`) where the loop variable then indexes `x`: prefer `eachindex(x)` (or `axes(x, d)`), which stays correct for collections whose indices are not one-based. The match is name-based — any `length`/`size` call counts — and, lacking type information to exempt collections that really are one-based like `Vector`, the rule is opinionated, so it only fires when the loop variable actually indexes the collection. The first shape carries an unsafe fix rewriting the `1:length`/`1:size` prefix to `eachindex`/`axes`: the rewrite is only value-equivalent when the collection's indices are one-based and dense, which cannot be proven without type information, so it needs `--unsafe-fixes`. Second, `for i in 3.5`: iterating a bare numeric literal runs the loop body once and is almost always a mistaken range; no fix, since the intended range is unknowable.

`1:length(x)` used to index `x`:

```julia
for i in 1:length(x)
    println(x[i])
end
```

```text
warning: index-from-length
 --> example.jl:1:10
  |
1 | for i in 1:length(x)
  |          ^^^^^^^^^^^ iterate `eachindex(x)` instead of `1:length(x)`
  = help: Replace `1:length` with `eachindex`
```

Iterating a bare number loops once:

```julia
for i in 3.5
    println(i)
end
```

```text
warning: index-from-length
 --> example.jl:1:10
  |
1 | for i in 3.5
  |          ^^^ iterating a numeric literal runs the loop body once; did you mean a range?
```
